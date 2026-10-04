//! The RIFF container (RFC 9649 section 2): reading a file into its frames
//! and metadata without decoding any pixels, and writing chunks.

use crate::Limits;
use crate::error::{Result, bitstream, limit};
use crate::lossless;

/// One chunk: its FourCC and payload (padding excluded).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Chunk<'a> {
    pub(crate) fourcc: [u8; 4],
    pub(crate) data: &'a [u8],
}

/// Splits `body` into chunks. A chunk whose declared size runs past the
/// end of `body` is an error; a missing final padding byte is tolerated.
pub(crate) fn chunks(body: &[u8]) -> Result<Vec<Chunk<'_>>> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at < body.len() {
        if body.len() - at < 8 {
            return Err(bitstream("a chunk header cut short by the end of the file"));
        }
        let fourcc: [u8; 4] = body[at..at + 4].try_into().unwrap();
        let size = u32::from_le_bytes(body[at + 4..at + 8].try_into().unwrap()) as usize;
        let start = at + 8;
        if size > body.len() - start {
            return Err(bitstream(format!(
                "chunk '{}' of {size} bytes runs past the end of the file",
                fourcc_name(&fourcc)
            )));
        }
        out.push(Chunk {
            fourcc,
            data: &body[start..start + size],
        });
        at = start + size + (size & 1);
    }
    Ok(out)
}

/// A FourCC for messages.
pub(crate) fn fourcc_name(f: &[u8; 4]) -> String {
    f.iter()
        .map(|&b| {
            if b.is_ascii_graphic() || b == b' ' {
                b as char
            } else {
                '?'
            }
        })
        .collect()
}

/// A frame's coded image.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Bitstream<'a> {
    /// A `VP8 ` chunk's payload.
    Lossy(&'a [u8]),
    /// A `VP8L` chunk's payload.
    Lossless(&'a [u8]),
}

/// One frame as stored: where it goes and its chunks.
#[derive(Clone, Debug)]
pub(crate) struct FrameRef<'a> {
    pub(crate) x: u32,
    pub(crate) y: u32,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) duration: u32,
    /// Alpha-blend onto the canvas (false: overwrite).
    pub(crate) blend: bool,
    /// Dispose to the background after display.
    pub(crate) dispose: bool,
    /// The `ALPH` chunk's payload (lossy frames only).
    pub(crate) alpha: Option<&'a [u8]>,
    pub(crate) bitstream: Bitstream<'a>,
}

/// The `ANIM` chunk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AnimParams {
    /// Background colour as stored: bytes blue, green, red, alpha.
    pub(crate) background_bgra: [u8; 4],
    pub(crate) loop_count: u16,
}

/// A parsed file.
#[derive(Clone, Debug)]
pub(crate) struct Parsed<'a> {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) extended: bool,
    /// The VP8X flags byte (0 for the simple formats).
    pub(crate) flags: u8,
    pub(crate) animated: bool,
    pub(crate) anim: Option<AnimParams>,
    pub(crate) frames: Vec<FrameRef<'a>>,
    pub(crate) icc: Option<&'a [u8]>,
    pub(crate) exif: Option<&'a [u8]>,
    pub(crate) xmp: Option<&'a [u8]>,
    pub(crate) unknown: Vec<Chunk<'a>>,
}

pub(crate) const FLAG_ICC: u8 = 0x20;
pub(crate) const FLAG_ALPHA: u8 = 0x10;
pub(crate) const FLAG_EXIF: u8 = 0x08;
pub(crate) const FLAG_XMP: u8 = 0x04;
pub(crate) const FLAG_ANIMATION: u8 = 0x02;

fn u24(b: &[u8]) -> u32 {
    u32::from(b[0]) | u32::from(b[1]) << 8 | u32::from(b[2]) << 16
}

/// The size a `VP8 ` payload declares (RFC 6386 section 9.1): it must be
/// a key frame, which is what a WebP image is.
pub(crate) fn vp8_dimensions(data: &[u8]) -> Result<(u32, u32)> {
    if data.len() < 10 {
        return Err(bitstream("VP8 frame shorter than a key frame header"));
    }
    if data[0] & 1 != 0 {
        return Err(bitstream(
            "VP8 chunk holds an inter frame; a WebP image is a key frame",
        ));
    }
    if data[3..6] != [0x9d, 0x01, 0x2a] {
        return Err(bitstream("VP8 key frame without its start code"));
    }
    let w = u32::from(u16::from_le_bytes([data[6], data[7]]) & 0x3fff);
    let h = u32::from(u16::from_le_bytes([data[8], data[9]]) & 0x3fff);
    if w == 0 || h == 0 {
        return Err(bitstream("VP8 frame of zero size"));
    }
    Ok((w, h))
}

fn bitstream_dimensions(b: &Bitstream<'_>) -> Result<(u32, u32)> {
    match b {
        Bitstream::Lossy(d) => vp8_dimensions(d),
        Bitstream::Lossless(d) => {
            let h = lossless::decode::read_header(d)?;
            Ok((h.width, h.height))
        }
    }
}

/// Reads the container: header, chunks, their order, every frame's place
/// and size. No pixels are decoded.
pub(crate) fn parse<'a>(data: &'a [u8], limits: &Limits) -> Result<Parsed<'a>> {
    if data.len() < 12 || &data[..4] != b"RIFF" || &data[8..12] != b"WEBP" {
        return Err(bitstream("not a WebP file (no RIFF....WEBP header)"));
    }
    let riff_size = u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;
    if riff_size < 4 {
        return Err(bitstream("RIFF size smaller than the WEBP FourCC"));
    }
    // Data after the RIFF chunk is ignored (section 2.4 lets readers); a
    // file shorter than the header says is read as far as it goes, and a
    // chunk cut short is an error below.
    let end = (8 + riff_size).min(data.len());
    let list = chunks(&data[12..end])?;
    let Some(first) = list.first() else {
        return Err(bitstream("WebP file with no chunks"));
    };
    let mut p = Parsed {
        width: 0,
        height: 0,
        extended: false,
        flags: 0,
        animated: false,
        anim: None,
        frames: Vec::new(),
        icc: None,
        exif: None,
        xmp: None,
        unknown: Vec::new(),
    };
    match &first.fourcc {
        b"VP8 " | b"VP8L" => {
            let bs = if &first.fourcc == b"VP8 " {
                Bitstream::Lossy(first.data)
            } else {
                Bitstream::Lossless(first.data)
            };
            let (w, h) = bitstream_dimensions(&bs)?;
            check_canvas(w, h, limits)?;
            p.width = w;
            p.height = h;
            p.frames.push(still(w, h, None, bs));
            // A simple file is one chunk; anything after it is ignored.
            return Ok(p);
        }
        b"VP8X" => {}
        other => {
            return Err(bitstream(format!(
                "WebP file starting with chunk '{}'",
                fourcc_name(other)
            )));
        }
    }
    let x = first.data;
    if x.len() < 10 {
        return Err(bitstream("VP8X chunk shorter than 10 bytes"));
    }
    p.extended = true;
    p.flags = x[0];
    p.animated = x[0] & FLAG_ANIMATION != 0;
    p.width = u24(&x[4..7]) + 1;
    p.height = u24(&x[7..10]) + 1;
    if u64::from(p.width) * u64::from(p.height) > u64::from(u32::MAX) {
        return Err(bitstream(format!(
            "canvas {}x{} has more than 2^32 - 1 pixels",
            p.width, p.height
        )));
    }
    check_canvas(p.width, p.height, limits)?;

    // The chunks that rebuild the image must come in the order ICCP, ANIM,
    // image data (section 2.7); metadata and unknown chunks may go anywhere.
    let mut stage = 0u8;
    let mut order = |rank: u8, name: &str| -> Result<()> {
        if rank < stage {
            return Err(bitstream(format!("'{name}' chunk out of order")));
        }
        stage = rank;
        Ok(())
    };
    let mut alpha: Option<&[u8]> = None;
    let mut image: Option<Bitstream<'_>> = None;
    for c in &list[1..] {
        match &c.fourcc {
            b"ICCP" => {
                order(1, "ICCP")?;
                p.icc.get_or_insert(c.data);
            }
            b"ANIM" if p.animated => {
                order(2, "ANIM")?;
                if c.data.len() < 6 {
                    return Err(bitstream("ANIM chunk shorter than 6 bytes"));
                }
                if p.anim.is_none() {
                    p.anim = Some(AnimParams {
                        background_bgra: c.data[..4].try_into().unwrap(),
                        loop_count: u16::from_le_bytes([c.data[4], c.data[5]]),
                    });
                }
            }
            b"ANMF" if p.animated => {
                order(3, "ANMF")?;
                if p.anim.is_none() {
                    return Err(bitstream("ANMF chunk before the ANIM chunk"));
                }
                if p.frames.len() as u64 >= u64::from(limits.max_frames) {
                    return Err(limit(format!("more than {} frames", limits.max_frames)));
                }
                p.frames.push(frame(c.data, p.width, p.height)?);
            }
            b"ALPH" if !p.animated => {
                order(3, "ALPH")?;
                if image.is_some() {
                    return Err(bitstream("ALPH chunk after the image data"));
                }
                alpha.get_or_insert(c.data);
            }
            b"VP8 " | b"VP8L" if !p.animated => {
                order(3, "image data")?;
                if image.is_none() {
                    image = Some(if &c.fourcc == b"VP8 " {
                        Bitstream::Lossy(c.data)
                    } else {
                        Bitstream::Lossless(c.data)
                    });
                }
            }
            b"EXIF" => {
                p.exif.get_or_insert(c.data);
            }
            b"XMP " => {
                p.xmp.get_or_insert(c.data);
            }
            // ANIM and ANMF in a file not flagged animated, and image
            // chunks at the top of an animated one, are not part of the
            // picture: ignored, as section 2.7.1.1 says for ANIM.
            b"ANIM" | b"ANMF" | b"ALPH" | b"VP8 " | b"VP8L" | b"VP8X" => {}
            _ => p.unknown.push(*c),
        }
    }
    if p.animated {
        if p.anim.is_none() {
            return Err(bitstream("animated WebP without an ANIM chunk"));
        }
        if p.frames.is_empty() {
            return Err(bitstream("animated WebP without frames"));
        }
    } else {
        let Some(bs) = image else {
            return Err(bitstream("extended WebP without image data"));
        };
        let (w, h) = bitstream_dimensions(&bs)?;
        if (w, h) != (p.width, p.height) {
            return Err(bitstream(format!(
                "image is {w}x{h} on a {}x{} canvas",
                p.width, p.height
            )));
        }
        // An ALPH chunk belongs to a lossy image; a lossless one carries
        // its own alpha (section 2.7.1.2).
        let alpha = if matches!(bs, Bitstream::Lossy(_)) {
            alpha
        } else {
            None
        };
        p.frames.push(still(w, h, alpha, bs));
    }
    Ok(p)
}

fn still<'a>(w: u32, h: u32, alpha: Option<&'a [u8]>, bitstream: Bitstream<'a>) -> FrameRef<'a> {
    FrameRef {
        x: 0,
        y: 0,
        width: w,
        height: h,
        duration: 0,
        blend: false,
        dispose: false,
        alpha,
        bitstream,
    }
}

fn check_canvas(w: u32, h: u32, limits: &Limits) -> Result<()> {
    let pixels = u64::from(w) * u64::from(h);
    if pixels > limits.max_pixels {
        return Err(limit(format!(
            "{w}x{h} is {pixels} pixels, the limit is {}",
            limits.max_pixels
        )));
    }
    Ok(())
}

/// An `ANMF` payload (section 2.7.1.1).
fn frame(d: &[u8], canvas_w: u32, canvas_h: u32) -> Result<FrameRef<'_>> {
    if d.len() < 16 {
        return Err(bitstream("ANMF chunk shorter than its 16-byte header"));
    }
    let x = u24(&d[0..3]) * 2;
    let y = u24(&d[3..6]) * 2;
    let width = u24(&d[6..9]) + 1;
    let height = u24(&d[9..12]) + 1;
    let duration = u24(&d[12..15]);
    let flags = d[15];
    if u64::from(x) + u64::from(width) > u64::from(canvas_w)
        || u64::from(y) + u64::from(height) > u64::from(canvas_h)
    {
        return Err(bitstream(format!(
            "frame {width}x{height} at ({x}, {y}) does not fit the {canvas_w}x{canvas_h} canvas"
        )));
    }
    let mut alpha = None;
    let mut image = None;
    for c in chunks(&d[16..])? {
        match &c.fourcc {
            b"ALPH" => {
                if image.is_some() {
                    return Err(bitstream("ALPH chunk after the frame's image data"));
                }
                alpha.get_or_insert(c.data);
            }
            b"VP8 " if image.is_none() => image = Some(Bitstream::Lossy(c.data)),
            b"VP8L" if image.is_none() => image = Some(Bitstream::Lossless(c.data)),
            _ => {}
        }
    }
    let Some(bs) = image else {
        return Err(bitstream("ANMF chunk without image data"));
    };
    let (w, h) = bitstream_dimensions(&bs)?;
    if (w, h) != (width, height) {
        return Err(bitstream(format!(
            "frame image is {w}x{h}, its ANMF header says {width}x{height}"
        )));
    }
    let alpha = if matches!(bs, Bitstream::Lossy(_)) {
        alpha
    } else {
        None
    };
    Ok(FrameRef {
        x,
        y,
        width,
        height,
        duration,
        blend: flags & 0x02 == 0,
        dispose: flags & 0x01 != 0,
        alpha,
        bitstream: bs,
    })
}

/// Appends a chunk (header, payload, padding) to `out`.
pub(crate) fn write_chunk(out: &mut Vec<u8>, fourcc: &[u8; 4], payload: &[u8]) {
    out.extend_from_slice(fourcc);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    if payload.len() & 1 == 1 {
        out.push(0);
    }
}

/// Wraps chunk bytes in the RIFF/WEBP header.
pub(crate) fn riff(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 12);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&((body.len() + 4) as u32).to_le_bytes());
    out.extend_from_slice(b"WEBP");
    out.extend_from_slice(body);
    out
}

/// A VP8X payload.
pub(crate) fn vp8x(flags: u8, width: u32, height: u32) -> [u8; 10] {
    let mut v = [0u8; 10];
    v[0] = flags;
    v[4..7].copy_from_slice(&(width - 1).to_le_bytes()[..3]);
    v[7..10].copy_from_slice(&(height - 1).to_le_bytes()[..3]);
    v
}
