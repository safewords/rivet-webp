//! A WebP decoder and encoder.
//!
//! Rust, no C, no system libraries, no build script. Written from RFC 9649
//! (*WebP Image Format*) — not from libwebp or any other implementation.
//! Lossy frames are VP8 (RFC 6386), coded by rivet-vp8, this project's own
//! VP8 codec.
//!
//! - **Decoding** ([`decode`], [`Decoder`]): the simple lossy (`VP8 `) and
//!   lossless (`VP8L`) formats and the extended format (`VP8X`): alpha
//!   (`ALPH`, raw or lossless, every filter), animation composited to
//!   whole RGBA canvases ([`Decoder::frames`]: blending, disposal,
//!   background, loop count, durations), and the `ICCP`, `EXIF`, `XMP `
//!   and unknown chunks handed back as they are ([`Info`]). The lossless
//!   decoder implements every transform (predictor, colour, subtract-green,
//!   colour indexing with pixel bundling), the colour cache, meta prefix
//!   codes and LZ77 backward references with the distance map.
//! - **Encoding** ([`encode`], [`AnimationEncoder`]): lossless VP8L with
//!   this crate's own encoder (transforms chosen per image, hash-chain
//!   LZ77, a sized colour cache, clustered meta prefix codes), exact to the
//!   bit; lossy VP8 through rivet-vp8 with alpha in a lossless `ALPH`
//!   chunk; animations; metadata.
//! - **Robustness**: malformed input is an [`Error`], never a panic;
//!   [`Limits`] bound the pixels, frames and compositing work a file can
//!   ask for, checked before anything is allocated.
//!
//! Pixels in and out are 8-bit RGBA, not premultiplied ([`Image`]).
//! Lossy pictures convert from and to Y'CbCr with BT.601 (RFC 9649
//! section 2.5).
//!

#![forbid(unsafe_code)]
#![warn(missing_docs)]
// Pixel loops index several arrays at once; index loops say that plainly.
#![allow(clippy::needless_range_loop)]

#[allow(dead_code)]
mod alpha;
#[allow(dead_code)]
mod bits;
mod container;
mod decoder;
mod error;
#[allow(dead_code)]
mod huffman;
mod lossless;
mod lossy;

pub use decoder::{DecodeOptions, Decoder, Format, Frame, Frames, Info, UnknownChunk};
pub use error::{Error, Result};

/// An RGBA picture: 8 bits a channel, not premultiplied, rows top to
/// bottom, no padding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height * 4` bytes: red, green, blue, alpha.
    pub rgba: Vec<u8>,
}

impl Image {
    /// An image from its RGBA bytes; fails if the length does not match.
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Result<Image> {
        if rgba.len() as u64 != u64::from(width) * u64::from(height) * 4 {
            return Err(error::invalid(format!(
                "RGBA buffer of {} bytes for {width}x{height} (needs {})",
                rgba.len(),
                u64::from(width) * u64::from(height) * 4
            )));
        }
        Ok(Image { width, height, rgba })
    }

    /// Whether any pixel is less than opaque.
    pub fn has_alpha(&self) -> bool {
        self.rgba.as_chunks::<4>().0.iter().any(|p| p[3] != 255)
    }
}

/// Bounds on what a file may make the decoder do. The defaults admit any
/// still WebP can code (16384 x 16384 lossless) and long animations, and
/// stop a few bytes from asking for gigabytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    /// The most pixels a canvas may have (default 2^28, 16384 x 16384).
    pub max_pixels: u64,
    /// The most frames an animation may have (default 100 000).
    pub max_frames: u32,
    /// The most canvas pixels compositing may produce over all frames
    /// (default 2^33).
    pub max_animation_pixels: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_pixels: 1 << 28,
            max_frames: 100_000,
            max_animation_pixels: 1 << 33,
        }
    }
}

/// Decodes a WebP file: a still image, or the first frame of an animation
/// as composited on its canvas.
pub fn decode(data: &[u8]) -> Result<Image> {
    Decoder::new(data)?.decode()
}

/// Reads a WebP file's description (size, alpha, animation, metadata)
/// without decoding pixels.
pub fn probe(data: &[u8]) -> Result<Info> {
    Ok(Decoder::new(data)?.info())
}
