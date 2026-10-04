//! The public encoder: stills (lossless or lossy with alpha) and
//! animations.

use crate::container::{
    FLAG_ALPHA, FLAG_ANIMATION, FLAG_EXIF, FLAG_ICC, FLAG_XMP, riff, vp8x, write_chunk,
};
use crate::error::{Result, invalid};
use crate::{Image, alpha, lossless, lossy};

/// Encoding settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncoderConfig {
    /// Lossless (VP8L) rather than lossy (VP8, with alpha in a lossless
    /// `ALPH` chunk).
    pub lossless: bool,
    /// Lossy quality, 1 (smallest) to 100 (best); ignored when lossless.
    pub quality: u8,
    /// Encoder effort, 0 (fastest) to 6 (smallest): how many lossless
    /// strategies are tried and how hard each searches. Applies to lossless
    /// images and to lossy images' alpha.
    pub effort: u8,
    /// Keep the colour of fully transparent pixels. When false, a lossless
    /// encode stores such pixels as transparent black (which compresses
    /// better, and is invisible); the alpha plane is exact either way.
    pub exact: bool,
    /// An ICC profile to store (`ICCP`).
    pub icc_profile: Option<Vec<u8>>,
    /// Exif data to store (`EXIF`).
    pub exif: Option<Vec<u8>>,
    /// An XMP packet to store (`XMP `).
    pub xmp: Option<Vec<u8>>,
}

impl Default for EncoderConfig {
    /// Lossy at quality 80, effort 4, exact, no metadata.
    fn default() -> Self {
        EncoderConfig {
            lossless: false,
            quality: 80,
            effort: 4,
            exact: true,
            icc_profile: None,
            exif: None,
            xmp: None,
        }
    }
}

impl EncoderConfig {
    /// Lossless at effort 4, exact.
    pub fn lossless() -> Self {
        EncoderConfig {
            lossless: true,
            ..Default::default()
        }
    }

    /// Lossy at `quality`.
    pub fn lossy(quality: u8) -> Self {
        EncoderConfig {
            quality,
            ..Default::default()
        }
    }

    fn check(&self) -> Result<()> {
        if !self.lossless && !(1..=100).contains(&self.quality) {
            return Err(invalid(format!("quality {} (1 to 100)", self.quality)));
        }
        if self.effort > 6 {
            return Err(invalid(format!("effort {} (0 to 6)", self.effort)));
        }
        Ok(())
    }

    fn has_metadata(&self) -> bool {
        self.icc_profile.is_some() || self.exif.is_some() || self.xmp.is_some()
    }
}

/// The largest side: VP8L codes 14 bits, VP8 slightly less.
fn max_side(lossless: bool) -> u32 {
    if lossless { 16384 } else { 16383 }
}

fn check_image(image: &Image, lossless: bool) -> Result<()> {
    if image.width == 0 || image.height == 0 {
        return Err(invalid(format!(
            "image of {}x{}",
            image.width, image.height
        )));
    }
    let m = max_side(lossless);
    if image.width > m || image.height > m {
        return Err(invalid(format!(
            "image of {}x{}: {} WebP is at most {m} pixels a side",
            image.width,
            image.height,
            if lossless { "lossless" } else { "lossy" }
        )));
    }
    let want = image.width as usize * image.height as usize * 4;
    if image.rgba.len() != want {
        return Err(invalid(format!(
            "RGBA buffer of {} bytes for {}x{} (needs {want})",
            image.rgba.len(),
            image.width,
            image.height
        )));
    }
    Ok(())
}

/// RGBA bytes to ARGB words, transparent pixels cleared unless `exact`.
fn to_argb(rgba: &[u8], exact: bool) -> Vec<u32> {
    rgba.as_chunks::<4>()
        .0
        .iter()
        .map(|p| {
            if !exact && p[3] == 0 {
                0
            } else {
                (u32::from(p[3]) << 24)
                    | (u32::from(p[0]) << 16)
                    | (u32::from(p[1]) << 8)
                    | u32::from(p[2])
            }
        })
        .collect()
}

/// A frame's coded chunks (`ALPH` + `VP8 `, or `VP8L`) and whether it has
/// alpha.
fn frame_chunks(image: &Image, config: &EncoderConfig) -> Result<(Vec<u8>, bool)> {
    let (w, h) = (image.width as usize, image.height as usize);
    let has_alpha = image.has_alpha();
    let mut out = Vec::new();
    if config.lossless {
        let data =
            lossless::encode::encode(&to_argb(&image.rgba, config.exact), w, h, config.effort);
        write_chunk(&mut out, b"VP8L", &data);
    } else {
        if has_alpha {
            let a: Vec<u8> = image.rgba.as_chunks::<4>().0.iter().map(|p| p[3]).collect();
            write_chunk(&mut out, b"ALPH", &alpha::encode(&a, w, h, config.effort));
        }
        let data = lossy::encode(&image.rgba, image.width, image.height, config.quality)?;
        write_chunk(&mut out, b"VP8 ", &data);
    }
    Ok((out, has_alpha))
}

/// Encodes a still image.
///
/// The simple format (one `VP8 ` or `VP8L` chunk) is written when it can
/// hold the picture; the extended format (`VP8X`) when there is metadata or
/// a lossy image has alpha.
pub fn encode(image: &Image, config: &EncoderConfig) -> Result<Vec<u8>> {
    config.check()?;
    check_image(image, config.lossless)?;
    let (chunks, has_alpha) = frame_chunks(image, config)?;
    if !config.has_metadata() && (config.lossless || !has_alpha) {
        return Ok(riff(&chunks));
    }
    let mut flags = 0;
    if has_alpha {
        flags |= FLAG_ALPHA;
    }
    let mut body = Vec::new();
    write_header_chunks(
        &mut body,
        &mut flags,
        config,
        image.width,
        image.height,
        None,
    );
    body.extend_from_slice(&chunks);
    write_metadata(&mut body, config);
    Ok(riff(&body))
}

/// VP8X (with `flags` completed by the metadata present), ICCP, ANIM.
fn write_header_chunks(
    body: &mut Vec<u8>,
    flags: &mut u8,
    config: &EncoderConfig,
    w: u32,
    h: u32,
    anim: Option<&AnimationOptions>,
) {
    if config.icc_profile.is_some() {
        *flags |= FLAG_ICC;
    }
    if config.exif.is_some() {
        *flags |= FLAG_EXIF;
    }
    if config.xmp.is_some() {
        *flags |= FLAG_XMP;
    }
    if anim.is_some() {
        *flags |= FLAG_ANIMATION;
    }
    write_chunk(body, b"VP8X", &vp8x(*flags, w, h));
    if let Some(icc) = &config.icc_profile {
        write_chunk(body, b"ICCP", icc);
    }
    if let Some(a) = anim {
        let [r, g, b, al] = a.background;
        let mut p = vec![b, g, r, al];
        p.extend_from_slice(&a.loop_count.to_le_bytes());
        write_chunk(body, b"ANIM", &p);
    }
}

fn write_metadata(body: &mut Vec<u8>, config: &EncoderConfig) {
    if let Some(e) = &config.exif {
        write_chunk(body, b"EXIF", e);
    }
    if let Some(x) = &config.xmp {
        write_chunk(body, b"XMP ", x);
    }
}

/// Animation settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnimationOptions {
    /// Times to play, 0 for forever.
    pub loop_count: u16,
    /// Background colour, RGBA (a hint to viewers).
    pub background: [u8; 4],
}

impl Default for AnimationOptions {
    /// Loop forever, transparent black background.
    fn default() -> Self {
        AnimationOptions {
            loop_count: 0,
            background: [0; 4],
        }
    }
}

/// Encodes an animation frame by frame.
///
/// Each frame is given whole (canvas-sized); the encoder stores only the
/// rectangle that changed since the previous frame, and within it, when
/// every changed pixel is opaque, marks unchanged pixels transparent and
/// alpha-blends the frame (whichever of the two codings is smaller is
/// kept, for lossless frames). The first frame covers the canvas without
/// blending, so the result does not depend on how a viewer initialises
/// the canvas. No frame disposes. Durations are in milliseconds, at most
/// 2^24 - 1.
///
/// ```
/// # fn main() -> webp::Result<()> {
/// let mut enc = webp::AnimationEncoder::new(16, 16, webp::EncoderConfig::lossless(), Default::default())?;
/// for i in 0..4u8 {
///     let mut rgba = vec![255u8; 16 * 16 * 4];
///     rgba[usize::from(i) * 4] = 0;
///     enc.add_frame(&webp::Image::new(16, 16, rgba)?, 100)?;
/// }
/// let file = enc.finish()?;
/// let frames: Vec<_> = webp::Decoder::new(&file)?.frames().collect::<Result<_, _>>()?;
/// assert_eq!(frames.len(), 4);
/// # Ok(()) }
/// ```
pub struct AnimationEncoder {
    width: u32,
    height: u32,
    config: EncoderConfig,
    options: AnimationOptions,
    /// ANMF chunks so far.
    body: Vec<u8>,
    frames: usize,
    any_alpha: bool,
    /// The previous source frame, for change detection.
    previous: Option<Vec<u8>>,
}

impl AnimationEncoder {
    /// An encoder for a `width` x `height` canvas.
    pub fn new(
        width: u32,
        height: u32,
        config: EncoderConfig,
        options: AnimationOptions,
    ) -> Result<Self> {
        config.check()?;
        if width == 0 || height == 0 || width > 1 << 24 || height > 1 << 24 {
            return Err(invalid(format!("canvas of {width}x{height}")));
        }
        Ok(AnimationEncoder {
            width,
            height,
            config,
            options,
            body: Vec::new(),
            frames: 0,
            any_alpha: false,
            previous: None,
        })
    }

    /// Adds a frame shown for `duration_ms`.
    pub fn add_frame(&mut self, image: &Image, duration_ms: u32) -> Result<()> {
        if (image.width, image.height) != (self.width, self.height) {
            return Err(invalid(format!(
                "frame of {}x{} for a {}x{} canvas",
                image.width, image.height, self.width, self.height
            )));
        }
        if duration_ms >= 1 << 24 {
            return Err(invalid(format!(
                "frame duration {duration_ms} ms (at most 2^24 - 1)"
            )));
        }
        let (cw, ch) = (self.width as usize, self.height as usize);
        if image.rgba.len() != cw * ch * 4 {
            return Err(invalid("RGBA buffer does not match the frame size"));
        }
        // (image, alpha-blend, rectangle) codings to choose from.
        let mut candidates: Vec<(Image, bool, Rect)> = Vec::new();
        match &self.previous {
            None => candidates.push((image.clone(), false, (0, 0, cw, ch))),
            Some(prev) => match changed_rect(prev, &image.rgba, cw, ch) {
                // Nothing changed: one transparent pixel, blended.
                None => candidates.push((
                    Image {
                        width: 1,
                        height: 1,
                        rgba: vec![0; 4],
                    },
                    true,
                    (0, 0, 1, 1),
                )),
                Some(r) => {
                    let sub = crop(&image.rgba, cw, r);
                    let before = crop(prev, cw, r);
                    // Blending, with unchanged pixels transparent, is
                    // exact when every changed pixel is opaque.
                    let blendable = self.config.lossless || self.config.effort >= 3;
                    if blendable
                        && sub
                            .as_chunks::<4>()
                            .0
                            .iter()
                            .zip(before.as_chunks::<4>().0.iter())
                            .all(|(s, b)| s == b || s[3] == 255)
                    {
                        let mut rgba = sub.clone();
                        for (s, b) in rgba
                            .as_chunks_mut::<4>()
                            .0
                            .iter_mut()
                            .zip(before.as_chunks::<4>().0.iter())
                        {
                            if s == b {
                                s.copy_from_slice(&[0, 0, 0, 0]);
                            }
                        }
                        candidates.push((rect_image(r, rgba), true, r));
                    }
                    candidates.push((rect_image(r, sub), false, r));
                }
            },
        }
        let mut best: Option<(Vec<u8>, bool, bool, Rect)> = None;
        for (img, blend, r) in &candidates {
            let m = max_side(self.config.lossless);
            if img.width > m || img.height > m {
                return Err(invalid(format!(
                    "a frame region of {}x{} exceeds the {m}-pixel side limit",
                    img.width, img.height
                )));
            }
            // The transparent pixels of a blended frame only need to stay
            // transparent; their colour is free.
            let mut cfg = self.config.clone();
            if *blend {
                cfg.exact = false;
            }
            let (chunks, has_alpha) = frame_chunks(img, &cfg)?;
            if best.as_ref().is_none_or(|b| chunks.len() < b.0.len()) {
                best = Some((chunks, *blend, has_alpha, *r));
            }
        }
        let Some((chunks, blend, has_alpha, (x0, y0, x1, y1))) = best else {
            return Err(invalid("no coding for the frame"));
        };
        let mut anmf = Vec::with_capacity(16 + chunks.len());
        anmf.extend_from_slice(&((x0 / 2) as u32).to_le_bytes()[..3]);
        anmf.extend_from_slice(&((y0 / 2) as u32).to_le_bytes()[..3]);
        anmf.extend_from_slice(&((x1 - x0 - 1) as u32).to_le_bytes()[..3]);
        anmf.extend_from_slice(&((y1 - y0 - 1) as u32).to_le_bytes()[..3]);
        anmf.extend_from_slice(&duration_ms.to_le_bytes()[..3]);
        anmf.push(if blend { 0 } else { 0x02 });
        anmf.extend_from_slice(&chunks);
        write_chunk(&mut self.body, b"ANMF", &anmf);
        self.any_alpha |= has_alpha;
        self.frames += 1;
        self.previous = Some(image.rgba.clone());
        Ok(())
    }

    /// The file.
    pub fn finish(self) -> Result<Vec<u8>> {
        if self.frames == 0 {
            return Err(invalid("an animation with no frames"));
        }
        let mut flags = 0;
        if self.any_alpha {
            flags |= FLAG_ALPHA;
        }
        let mut body = Vec::new();
        write_header_chunks(
            &mut body,
            &mut flags,
            &self.config,
            self.width,
            self.height,
            Some(&self.options),
        );
        body.extend_from_slice(&self.body);
        write_metadata(&mut body, &self.config);
        if body.len() as u64 + 4 > u64::from(u32::MAX) {
            return Err(invalid("animation larger than a RIFF file can hold"));
        }
        Ok(riff(&body))
    }
}

/// A frame rectangle: x0, y0, x1, y1 (exclusive).
type Rect = (usize, usize, usize, usize);

fn crop(rgba: &[u8], canvas_width: usize, (x0, y0, x1, y1): Rect) -> Vec<u8> {
    let mut out = Vec::with_capacity((x1 - x0) * (y1 - y0) * 4);
    for y in y0..y1 {
        out.extend_from_slice(&rgba[(y * canvas_width + x0) * 4..(y * canvas_width + x1) * 4]);
    }
    out
}

fn rect_image((x0, y0, x1, y1): Rect, rgba: Vec<u8>) -> Image {
    Image {
        width: (x1 - x0) as u32,
        height: (y1 - y0) as u32,
        rgba,
    }
}

/// The bounding box (x0, y0, x1, y1) of the pixels that differ, with x0 and
/// y0 rounded down to even; `None` when nothing differs.
fn changed_rect(a: &[u8], b: &[u8], w: usize, h: usize) -> Option<Rect> {
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
    for y in 0..h {
        let ra = &a[y * w * 4..(y + 1) * w * 4];
        let rb = &b[y * w * 4..(y + 1) * w * 4];
        if ra == rb {
            continue;
        }
        y0 = y0.min(y);
        y1 = y + 1;
        let first = (0..w)
            .find(|&x| ra[x * 4..x * 4 + 4] != rb[x * 4..x * 4 + 4])
            .unwrap_or(0);
        let last = (0..w)
            .rev()
            .find(|&x| ra[x * 4..x * 4 + 4] != rb[x * 4..x * 4 + 4])
            .unwrap_or(w - 1);
        x0 = x0.min(first);
        x1 = x1.max(last + 1);
    }
    if y0 == usize::MAX {
        return None;
    }
    Some((x0 & !1, y0 & !1, x1, y1))
}

// Keep the compositor and the encoder's assumption about it in one place:
// a transparent source pixel leaves the canvas as it was.
#[cfg(test)]
mod tests {
    use crate::decoder::blend;

    #[test]
    fn transparent_blend_keeps_the_canvas() {
        let mut d = [1, 2, 3, 4];
        blend(&mut d, &[9, 9, 9, 0]);
        assert_eq!(d, [1, 2, 3, 4]);
    }
}
