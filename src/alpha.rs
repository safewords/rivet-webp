//! The `ALPH` chunk (RFC 9649 section 2.7.1.2): an alpha plane stored raw
//! or as a headerless lossless image-stream (alpha in the green channel),
//! after one of three spatial filters.

use crate::error::{Result, bitstream};
use crate::lossless;

/// The header byte's fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AlphaHeader {
    /// 0 raw, 1 lossless.
    pub(crate) compression: u8,
    /// 0 none, 1 horizontal, 2 vertical, 3 gradient.
    pub(crate) filter: u8,
    /// 0 none, 1 level reduction (informative only).
    pub(crate) preprocessing: u8,
}

impl AlphaHeader {
    /// Bit numbering is MSB 0 (section 2.2): `Rsv(2) P(2) F(2) C(2)` from
    /// the top, so compression is the low two bits.
    pub(crate) fn parse(b: u8) -> AlphaHeader {
        AlphaHeader {
            compression: b & 3,
            filter: (b >> 2) & 3,
            preprocessing: (b >> 4) & 3,
        }
    }

    pub(crate) fn byte(self) -> u8 {
        (self.preprocessing << 4) | (self.filter << 2) | self.compression
    }
}

/// Decodes an `ALPH` payload to `width * height` alpha values.
pub(crate) fn decode(data: &[u8], width: usize, height: usize) -> Result<Vec<u8>> {
    let Some((&first, rest)) = data.split_first() else {
        return Err(bitstream("empty ALPH chunk"));
    };
    let h = AlphaHeader::parse(first);
    let n = width * height;
    let mut a = match h.compression {
        0 => {
            if rest.len() < n {
                return Err(bitstream(format!(
                    "raw ALPH data is {} bytes, the frame needs {n}",
                    rest.len()
                )));
            }
            rest[..n].to_vec()
        }
        1 => {
            let argb = lossless::decode::decode_headerless(rest, width, height)?;
            argb.iter().map(|&p| (p >> 8) as u8).collect()
        }
        c => {
            return Err(bitstream(format!(
                "ALPH compression method {c} (0 or 1 defined)"
            )));
        }
    };
    unfilter(&mut a, width, height, h.filter);
    Ok(a)
}

/// The filter's prediction for (x, y) from the values already known, with
/// the RFC's edge rules: (0, 0) predicts 0; the top row predicts from the
/// left and the left column from above, for every method but none.
#[inline(always)]
fn prediction(a: &[u8], width: usize, x: usize, y: usize, filter: u8) -> u8 {
    let i = y * width + x;
    if filter == 0 {
        return 0;
    }
    if y == 0 {
        return if x == 0 { 0 } else { a[i - 1] };
    }
    if x == 0 {
        return a[i - width];
    }
    match filter {
        1 => a[i - 1],
        2 => a[i - width],
        _ => {
            let g = i32::from(a[i - 1]) + i32::from(a[i - width]) - i32::from(a[i - width - 1]);
            g.clamp(0, 255) as u8
        }
    }
}

/// Undoes `filter` in place.
pub(crate) fn unfilter(a: &mut [u8], width: usize, height: usize, filter: u8) {
    if filter == 0 {
        return;
    }
    for y in 0..height {
        for x in 0..width {
            let p = prediction(a, width, x, y, filter);
            let i = y * width + x;
            a[i] = a[i].wrapping_add(p);
        }
    }
}

/// Applies `filter`: each value minus its prediction from the originals.
pub(crate) fn filter(a: &[u8], width: usize, height: usize, filter: u8) -> Vec<u8> {
    let mut out = vec![0u8; a.len()];
    for y in 0..height {
        for x in 0..width {
            let i = y * width + x;
            out[i] = a[i].wrapping_sub(prediction(a, width, x, y, filter));
        }
    }
    out
}

/// Encodes an alpha plane as an `ALPH` payload: each filter `effort`
/// allows is tried with lossless compression, and raw storage too; the
/// smallest wins.
pub(crate) fn encode(a: &[u8], width: usize, height: usize, effort: u8) -> Vec<u8> {
    let filters: &[u8] = match effort {
        0 => &[0],
        1..=2 => &[0, 3],
        _ => &[0, 1, 2, 3],
    };
    let mut best: Vec<u8> = Vec::with_capacity(a.len() + 1);
    best.push(
        AlphaHeader {
            compression: 0,
            filter: 0,
            preprocessing: 0,
        }
        .byte(),
    );
    best.extend_from_slice(a);
    for &f in filters {
        let filtered = filter(a, width, height, f);
        let argb: Vec<u32> = filtered
            .iter()
            .map(|&v| 0xff000000 | (u32::from(v) << 8))
            .collect();
        let stream = lossless::encode::encode_headerless(&argb, width, height, effort, true);
        if stream.len() + 1 < best.len() {
            best.clear();
            best.push(
                AlphaHeader {
                    compression: 1,
                    filter: f,
                    preprocessing: 0,
                }
                .byte(),
            );
            best.extend_from_slice(&stream);
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_round_trip() {
        let (w, h) = (17, 9);
        let a: Vec<u8> = (0..w * h).map(|i| ((i * 37) ^ (i >> 3)) as u8).collect();
        for f in 0..4 {
            let mut x = filter(&a, w, h, f);
            unfilter(&mut x, w, h, f);
            assert_eq!(x, a, "filter {f}");
        }
    }

    #[test]
    fn header_bits() {
        let h = AlphaHeader::parse(0b0001_1101);
        assert_eq!(
            h,
            AlphaHeader {
                compression: 1,
                filter: 3,
                preprocessing: 1
            }
        );
        assert_eq!(h.byte(), 0b0001_1101);
    }
}
