//! The crate's error type.

use std::fmt;

/// Errors the decoder and the encoder can report.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// The data breaks the WebP syntax: a RIFF header that is not
    /// `RIFF....WEBP`, a chunk that runs past the end of the file, chunks out
    /// of the order RFC 9649 requires, a lossless bitstream with an
    /// incomplete prefix code or a backward reference before the start of
    /// the image, a frame outside the canvas.
    Bitstream(String),
    /// Valid data this crate does not implement, named: a lossless
    /// bitstream version other than 0.
    Unsupported(String),
    /// The picture is larger than the [`Limits`](crate::Limits) allow: too
    /// many pixels, too many frames, too much compositing work.
    LimitExceeded(String),
    /// Input the caller supplied that cannot be coded: a zero dimension, a
    /// buffer whose length does not match its dimensions, a quality out of
    /// range, a frame of the wrong size.
    InvalidInput(String),
    /// The lossy (VP8) bitstream, as rivet-vp8 reported it.
    Vp8(vp8::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Bitstream(m) => write!(f, "invalid WebP data: {m}"),
            Error::Unsupported(m) => write!(f, "unsupported WebP feature: {m}"),
            Error::LimitExceeded(m) => write!(f, "WebP picture over the decoder's limits: {m}"),
            Error::InvalidInput(m) => write!(f, "invalid input to the WebP encoder: {m}"),
            Error::Vp8(e) => write!(f, "WebP lossy (VP8) bitstream: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Vp8(e) => Some(e),
            _ => None,
        }
    }
}

impl From<vp8::Error> for Error {
    fn from(e: vp8::Error) -> Self {
        Error::Vp8(e)
    }
}

/// `Result` with this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

// Cold and out of line: errors are built on the failure paths of hot decoding
// loops, and the String construction does not belong inlined there.
#[cold]
#[inline(never)]
pub(crate) fn bitstream(msg: impl Into<String>) -> Error {
    Error::Bitstream(msg.into())
}

#[cold]
#[inline(never)]
pub(crate) fn unsupported(msg: impl Into<String>) -> Error {
    Error::Unsupported(msg.into())
}

#[cold]
#[inline(never)]
pub(crate) fn limit(msg: impl Into<String>) -> Error {
    Error::LimitExceeded(msg.into())
}

#[cold]
#[inline(never)]
pub(crate) fn invalid(msg: impl Into<String>) -> Error {
    Error::InvalidInput(msg.into())
}
