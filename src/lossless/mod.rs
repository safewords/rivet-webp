//! The WebP lossless bitstream, VP8L (RFC 9649 section 3): the decoder, the
//! encoder, and what they share.

pub(crate) mod decode;
pub(crate) mod transform;

/// The signature byte that opens a VP8L bitstream (section 3.4).
pub(crate) const SIGNATURE: u8 = 0x2f;

/// Length prefix codes: the green alphabet's symbols 256..280.
pub(crate) const NUM_LENGTH_CODES: usize = 24;
/// Distance prefix codes.
pub(crate) const NUM_DISTANCE_CODES: usize = 40;
/// Literal alphabets (red, blue, alpha; green's first part).
pub(crate) const NUM_LITERALS: usize = 256;
/// The longest backward reference (section 3.6.2.2).
pub(crate) const MAX_LENGTH: usize = 4096;
/// The largest distance code: prefix code 39 with all 18 extra bits set.
pub(crate) const MAX_DISTANCE_CODE: usize = 1_048_576;
/// Distance codes 1..=120 name neighbours; larger ones are a scan-line
/// distance plus this.
pub(crate) const PLANE_CODES: usize = 120;

/// The order in which code length code lengths are sent (section
/// 3.7.2.1.2).
pub(crate) const CODE_LENGTH_ORDER: [usize; 19] = [17, 18, 0, 1, 2, 3, 4, 5, 16, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

/// The colour cache's hash multiplier (section 3.6.2.3).
pub(crate) const CACHE_MULTIPLIER: u32 = 0x1e35a7bd;

/// A colour's slot in a cache of `1 << bits` entries.
#[inline(always)]
pub(crate) fn cache_index(argb: u32, bits: u32) -> usize {
    (CACHE_MULTIPLIER.wrapping_mul(argb) >> (32 - bits)) as usize
}

/// Distance codes 1..=120: (xi, yi), the neighbour xi columns to the left
/// (negative: to the right) and yi rows up (section 3.6.2.2.1, figure 20).
pub(crate) const DISTANCE_MAP: [(i8, i8); PLANE_CODES] = [
    (0, 1), (1, 0), (1, 1), (-1, 1), (0, 2), (2, 0), (1, 2),
    (-1, 2), (2, 1), (-2, 1), (2, 2), (-2, 2), (0, 3), (3, 0),
    (1, 3), (-1, 3), (3, 1), (-3, 1), (2, 3), (-2, 3), (3, 2),
    (-3, 2), (0, 4), (4, 0), (1, 4), (-1, 4), (4, 1), (-4, 1),
    (3, 3), (-3, 3), (2, 4), (-2, 4), (4, 2), (-4, 2), (0, 5),
    (3, 4), (-3, 4), (4, 3), (-4, 3), (5, 0), (1, 5), (-1, 5),
    (5, 1), (-5, 1), (2, 5), (-2, 5), (5, 2), (-5, 2), (4, 4),
    (-4, 4), (3, 5), (-3, 5), (5, 3), (-5, 3), (0, 6), (6, 0),
    (1, 6), (-1, 6), (6, 1), (-6, 1), (2, 6), (-2, 6), (6, 2),
    (-6, 2), (4, 5), (-4, 5), (5, 4), (-5, 4), (3, 6), (-3, 6),
    (6, 3), (-6, 3), (0, 7), (7, 0), (1, 7), (-1, 7), (5, 5),
    (-5, 5), (7, 1), (-7, 1), (4, 6), (-4, 6), (6, 4), (-6, 4),
    (2, 7), (-2, 7), (7, 2), (-7, 2), (3, 7), (-3, 7), (7, 3),
    (-7, 3), (5, 6), (-5, 6), (6, 5), (-6, 5), (8, 0), (4, 7),
    (-4, 7), (7, 4), (-7, 4), (8, 1), (8, 2), (6, 6), (-6, 6),
    (8, 3), (5, 7), (-5, 7), (7, 5), (-7, 5), (8, 4), (6, 7),
    (-6, 7), (7, 6), (-7, 6), (8, 5), (7, 7), (-7, 7), (8, 6),
    (8, 7),
];

/// The scan-line distance a distance code means in an image `width` wide.
#[inline]
pub(crate) fn code_to_distance(code: usize, width: usize) -> usize {
    if code > PLANE_CODES {
        code - PLANE_CODES
    } else {
        let (xi, yi) = DISTANCE_MAP[code - 1];
        let d = i64::from(xi) + i64::from(yi) * width as i64;
        d.max(1) as usize
    }
}

/// A value (length or distance code, 1-based) as its prefix code, the
/// number of extra bits and their value (section 3.6.2.2, the inverse of
/// the RFC's pseudocode).
#[inline]
pub(crate) fn prefix_encode(value: usize) -> (usize, u32, u32) {
    debug_assert!(value >= 1);
    if value <= 4 {
        return (value - 1, 0, 0);
    }
    let d = (value - 1) as u32;
    let high = 31 - d.leading_zeros();
    let second = (d >> (high - 1)) & 1;
    let extra_bits = high - 1;
    ((2 * high + second) as usize, extra_bits, d & ((1 << extra_bits) - 1))
}

/// The base value of a prefix code and its number of extra bits: the
/// value is `base + extra + 1` (section 3.6.2.2's pseudocode).
#[inline]
pub(crate) fn prefix_base(code: usize) -> (usize, u32) {
    if code < 4 {
        return (code, 0);
    }
    let extra_bits = ((code - 2) >> 1) as u32;
    ((2 + (code & 1)) << extra_bits, extra_bits)
}

/// `ceil(num / (1 << bits))`.
#[inline]
pub(crate) fn subsample(num: usize, bits: u32) -> usize {
    (num + (1 << bits) - 1) >> bits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_coding_inverts() {
        for v in 1..=MAX_DISTANCE_CODE {
            let (code, bits, extra) = prefix_encode(v);
            assert!(code < NUM_DISTANCE_CODES);
            let (base, b) = prefix_base(code);
            assert_eq!(b, bits);
            assert_eq!(base + extra as usize + 1, v, "value {v}");
        }
        // Table 4's rows. Its "3072..4096 -> 23" disagrees with the RFC's
        // own pseudocode, under which 3072 is code 22's last value
        // (docs/PROVENANCE.md); the pseudocode is what decoders run.
        assert_eq!(prefix_encode(4096).0, 23);
        assert_eq!(prefix_encode(3073).0, 23);
        assert_eq!(prefix_encode(3072).0, 22);
        assert_eq!(prefix_encode(786_433), (39, 18, 0));
    }

    #[test]
    fn neighbour_codes() {
        assert_eq!(code_to_distance(1, 100), 100);
        assert_eq!(code_to_distance(2, 100), 1);
        assert_eq!(code_to_distance(3, 100), 101);
        assert_eq!(code_to_distance(4, 100), 99);
        // (-1, 1) in an image one pixel wide is distance 0, raised to 1.
        assert_eq!(code_to_distance(4, 1), 1);
        assert_eq!(code_to_distance(121, 100), 1);
    }
}
