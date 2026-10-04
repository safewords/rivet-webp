//! The public decoder: stills, animation compositing, metadata.

use crate::container::{self, AnimParams, Bitstream, FrameRef, Parsed};
use crate::error::{Result, bitstream, limit};
use crate::{Image, Limits, alpha, lossless, lossy};

/// How the frames of a file are coded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// Every frame is lossy (`VP8 `, with or without `ALPH`).
    Lossy,
    /// Every frame is lossless (`VP8L`).
    Lossless,
    /// An animation with frames of both kinds.
    Mixed,
}

/// A chunk this crate does not interpret, kept in file order (RFC 9649
/// section 2.7.1.6 asks writers to preserve them).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownChunk {
    /// The chunk's FourCC.
    pub fourcc: [u8; 4],
    /// Its payload.
    pub data: Vec<u8>,
}

/// What a file holds, read from its chunks without decoding pixels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Info {
    /// Canvas width.
    pub width: u32,
    /// Canvas height.
    pub height: u32,
    /// Whether the picture may be transparent: the `VP8X` alpha flag, the
    /// lossless header's alpha hint, an `ALPH` chunk, or (animations) a
    /// background colour that is not opaque. A hint: a file can claim
    /// alpha and have none.
    pub has_alpha: bool,
    /// Whether the file is an animation (`VP8X` animation flag).
    pub animated: bool,
    /// Frames: 1 for a still.
    pub frame_count: usize,
    /// Times to play an animation, 0 for forever (1 for a still).
    pub loop_count: u16,
    /// An animation's background colour, RGBA (transparent black for a
    /// still). A hint, RFC 9649 says; see [`DecodeOptions::use_background`].
    pub background: [u8; 4],
    /// Sum of the frame durations, milliseconds.
    pub duration_ms: u64,
    /// Lossy, lossless or both.
    pub format: Format,
    /// Whether the file uses the extended format (`VP8X`).
    pub extended: bool,
    /// The `ICCP` chunk's colour profile.
    pub icc_profile: Option<Vec<u8>>,
    /// The `EXIF` chunk's Exif data.
    pub exif: Option<Vec<u8>>,
    /// The `XMP ` chunk's XMP packet.
    pub xmp: Option<Vec<u8>>,
    /// Unknown top-level chunks, in file order.
    pub unknown_chunks: Vec<UnknownChunk>,
}

/// Decoding settings.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DecodeOptions {
    /// Size and work bounds.
    pub limits: Limits,
    /// Whether an animation's canvas starts as (and disposes to) the
    /// `ANIM` background colour. Off by default: the canvas is transparent
    /// black, as browsers show it; RFC 9649 makes the colour a hint.
    pub use_background: bool,
}

/// A composited animation frame: the whole canvas as shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    /// The canvas, RGBA.
    pub image: Image,
    /// How long it is shown, milliseconds.
    pub duration_ms: u32,
    /// When it is first shown, milliseconds from the start.
    pub timestamp_ms: u64,
}

/// A WebP decoder over a file in memory. The container is read (and
/// checked) by [`Decoder::new`]; pixels are decoded on demand.
///
/// ```no_run
/// # fn main() -> webp::Result<()> {
/// let data = std::fs::read("in.webp").unwrap();
/// let dec = webp::Decoder::new(&data)?;
/// if dec.info().animated {
///     for frame in dec.frames() {
///         let frame = frame?;
///         println!("{} ms", frame.duration_ms);
///     }
/// } else {
///     let image = dec.decode()?;
///     assert_eq!(image.rgba.len(), image.width as usize * image.height as usize * 4);
/// }
/// # Ok(()) }
/// ```
pub struct Decoder<'a> {
    parsed: Parsed<'a>,
    options: DecodeOptions,
}

impl<'a> Decoder<'a> {
    /// Reads the container with the default options.
    pub fn new(data: &'a [u8]) -> Result<Self> {
        Self::with_options(data, DecodeOptions::default())
    }

    /// Reads the container.
    pub fn with_options(data: &'a [u8], options: DecodeOptions) -> Result<Self> {
        let parsed = container::parse(data, &options.limits)?;
        Ok(Decoder { parsed, options })
    }

    /// What the file holds.
    pub fn info(&self) -> Info {
        let p = &self.parsed;
        let lossy = p
            .frames
            .iter()
            .filter(|f| matches!(f.bitstream, Bitstream::Lossy(_)))
            .count();
        let format = if lossy == p.frames.len() {
            Format::Lossy
        } else if lossy == 0 {
            Format::Lossless
        } else {
            Format::Mixed
        };
        let anim = p.anim.unwrap_or(AnimParams {
            background_bgra: [0; 4],
            loop_count: 1,
        });
        let [b, g, r, a] = if p.animated {
            anim.background_bgra
        } else {
            [0; 4]
        };
        let frame_alpha = p.frames.iter().any(|f| {
            f.alpha.is_some()
                || match f.bitstream {
                    Bitstream::Lossless(d) => {
                        lossless::decode::read_header(d).is_ok_and(|h| h.alpha_hint)
                    }
                    Bitstream::Lossy(_) => false,
                }
        });
        Info {
            width: p.width,
            height: p.height,
            has_alpha: p.flags & container::FLAG_ALPHA != 0
                || frame_alpha
                || (p.animated && a != 255)
                || frames_leave_gaps(p),
            animated: p.animated,
            frame_count: p.frames.len(),
            loop_count: if p.animated { anim.loop_count } else { 1 },
            background: [r, g, b, a],
            duration_ms: p.frames.iter().map(|f| u64::from(f.duration)).sum(),
            format,
            extended: p.extended,
            icc_profile: p.icc.map(<[u8]>::to_vec),
            exif: p.exif.map(<[u8]>::to_vec),
            xmp: p.xmp.map(<[u8]>::to_vec),
            unknown_chunks: p
                .unknown
                .iter()
                .map(|c| UnknownChunk {
                    fourcc: c.fourcc,
                    data: c.data.to_vec(),
                })
                .collect(),
        }
    }

    /// The picture: a still image, or an animation's first frame as
    /// composited on its canvas.
    pub fn decode(&self) -> Result<Image> {
        if self.parsed.animated {
            return match self.frames().next() {
                Some(f) => f.map(|f| f.image),
                None => Err(bitstream("animation without frames")),
            };
        }
        let f = &self.parsed.frames[0];
        let rgba = decode_frame(f)?;
        Ok(Image {
            width: f.width,
            height: f.height,
            rgba,
        })
    }

    /// The frames of an animation, each composited onto the canvas (a
    /// still gives one frame).
    pub fn frames(&self) -> Frames<'_, 'a> {
        Frames {
            dec: self,
            next: 0,
            canvas: Vec::new(),
            timestamp: 0,
            work: 0,
        }
    }
}

/// Whether some canvas pixel is never covered by a frame, so shows the
/// (transparent) background.
fn frames_leave_gaps(p: &Parsed<'_>) -> bool {
    p.animated
        && !p
            .frames
            .iter()
            .any(|f| f.x == 0 && f.y == 0 && f.width == p.width && f.height == p.height)
}

/// One frame's pixels, RGBA, at the frame's own size.
fn decode_frame(f: &FrameRef<'_>) -> Result<Vec<u8>> {
    match f.bitstream {
        Bitstream::Lossless(d) => {
            let (h, argb) = lossless::decode::decode(d, u64::MAX)?;
            debug_assert_eq!((h.width, h.height), (f.width, f.height));
            // ARGB words to RGBA bytes: red and blue trade places.
            let mut out = vec![0u8; argb.len() * 4];
            crate::simd::with_wide_vectors(|| {
                for (o, &p) in out.as_chunks_mut::<4>().0.iter_mut().zip(&argb) {
                    *o = ((p & 0xff00ff00) | ((p >> 16) & 0xff) | ((p & 0xff) << 16)).to_le_bytes();
                }
            });
            Ok(out)
        }
        Bitstream::Lossy(d) => {
            let mut rgba = lossy::decode(d, f.width, f.height)?;
            if let Some(a) = f.alpha {
                let a = alpha::decode(a, f.width as usize, f.height as usize)?;
                for (px, &v) in rgba.as_chunks_mut::<4>().0.iter_mut().zip(&a) {
                    px[3] = v;
                }
            }
            Ok(rgba)
        }
    }
}

/// Iterator over composited frames ([`Decoder::frames`]). After an error it
/// ends.
pub struct Frames<'d, 'a> {
    dec: &'d Decoder<'a>,
    next: usize,
    canvas: Vec<u8>,
    timestamp: u64,
    work: u64,
}

impl Iterator for Frames<'_, '_> {
    type Item = Result<Frame>;

    fn next(&mut self) -> Option<Result<Frame>> {
        let p = &self.dec.parsed;
        let f = p.frames.get(self.next)?;
        let r = self.step(f);
        self.next = if r.is_err() {
            usize::MAX
        } else {
            self.next + 1
        };
        Some(r)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let left = self.dec.parsed.frames.len().saturating_sub(self.next);
        (left, Some(left))
    }
}

impl Frames<'_, '_> {
    fn background(&self) -> [u8; 4] {
        match (self.dec.options.use_background, self.dec.parsed.anim) {
            (true, Some(a)) => {
                let [b, g, r, al] = a.background_bgra;
                [r, g, b, al]
            }
            _ => [0; 4],
        }
    }

    fn step(&mut self, f: &FrameRef<'_>) -> Result<Frame> {
        let p = &self.dec.parsed;
        let (cw, ch) = (p.width as usize, p.height as usize);
        self.work += (cw * ch) as u64;
        if self.work > self.dec.options.limits.max_animation_pixels {
            return Err(limit(format!(
                "compositing past frame {} exceeds {} canvas pixels",
                self.next, self.dec.options.limits.max_animation_pixels
            )));
        }
        let bg = self.background();
        if self.next == 0 {
            self.canvas = bg.repeat(cw * ch);
        } else if let Some(prev) = p.frames.get(self.next - 1)
            && prev.dispose
        {
            for y in prev.y as usize..(prev.y + prev.height) as usize {
                for px in self.canvas
                    [(y * cw + prev.x as usize) * 4..(y * cw + (prev.x + prev.width) as usize) * 4]
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                {
                    px.copy_from_slice(&bg);
                }
            }
        }
        let rgba = decode_frame(f)?;
        let fw = f.width as usize;
        for y in 0..f.height as usize {
            let src = &rgba[y * fw * 4..(y + 1) * fw * 4];
            let at = ((f.y as usize + y) * cw + f.x as usize) * 4;
            let dst = &mut self.canvas[at..at + fw * 4];
            if f.blend {
                for (d, s) in dst
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .zip(src.as_chunks::<4>().0.iter())
                {
                    blend(d, s);
                }
            } else {
                dst.copy_from_slice(src);
            }
        }
        let frame = Frame {
            image: Image {
                width: p.width,
                height: p.height,
                rgba: self.canvas.clone(),
            },
            duration_ms: f.duration,
            timestamp_ms: self.timestamp,
        };
        self.timestamp += u64::from(f.duration);
        Ok(frame)
    }
}

/// RFC 9649's alpha-blending of `src` over `dst` (section 2.7.1.1), in
/// integers, non-premultiplied, rounded to nearest. Opaque and fully
/// transparent sources are exact (replace and keep).
#[inline]
pub(crate) fn blend(dst: &mut [u8], src: &[u8]) {
    let sa = u32::from(src[3]);
    if sa == 255 {
        dst.copy_from_slice(src);
        return;
    }
    if sa == 0 {
        return;
    }
    let da = u32::from(dst[3]);
    // Everything times 255: blend.A * 255 = src.A * 255 + dst.A * (255 - src.A).
    let dw = da * (255 - sa);
    let a255 = sa * 255 + dw;
    if a255 == 0 {
        dst.copy_from_slice(&[0, 0, 0, 0]);
        return;
    }
    for c in 0..3 {
        let num = u32::from(src[c]) * sa * 255 + u32::from(dst[c]) * dw;
        dst[c] = ((num + a255 / 2) / a255) as u8;
    }
    dst[3] = ((a255 + 127) / 255) as u8;
}

#[cfg(test)]
mod tests {
    use super::blend;

    #[test]
    fn blending() {
        let mut d = [10, 20, 30, 255];
        blend(&mut d, &[200, 100, 0, 0]);
        assert_eq!(d, [10, 20, 30, 255]);
        blend(&mut d, &[200, 100, 0, 255]);
        assert_eq!(d, [200, 100, 0, 255]);
        let mut d = [0, 0, 0, 0];
        blend(&mut d, &[255, 0, 0, 128]);
        assert_eq!(d, [255, 0, 0, 128]);
        let mut d = [0, 0, 255, 255];
        blend(&mut d, &[255, 0, 0, 128]);
        assert_eq!(d, [128, 0, 127, 255]);
    }
}
