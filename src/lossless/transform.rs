//! The four transforms of RFC 9649 section 3.5, both ways: the inverse the
//! decoder applies and the forward one the encoder applies. Pixels are ARGB
//! in a `u32` (alpha in bits 31..24, blue in 7..0), as in the RFC.

use super::subsample;

/// Adds two pixels channel by channel, modulo 256.
#[inline(always)]
pub(crate) fn add_pixels(a: u32, b: u32) -> u32 {
    let ag = (a & 0xff00ff00).wrapping_add(b & 0xff00ff00) & 0xff00ff00;
    let rb = (a & 0x00ff00ff).wrapping_add(b & 0x00ff00ff) & 0x00ff00ff;
    ag | rb
}

/// Subtracts `b` from `a` channel by channel, modulo 256.
#[inline(always)]
pub(crate) fn sub_pixels(a: u32, b: u32) -> u32 {
    let ag = (0x00ff00ff | (a & 0xff00ff00)).wrapping_sub(b & 0xff00ff00) & 0xff00ff00;
    let rb = (0xff00ff00 | (a & 0x00ff00ff)).wrapping_sub(b & 0x00ff00ff) & 0x00ff00ff;
    ag | rb
}

/// `Average2` of every channel: `(a + b) / 2`, rounded down.
#[inline(always)]
fn average2(a: u32, b: u32) -> u32 {
    (((a ^ b) & 0xfefefefe) >> 1) + (a & b)
}

#[cfg(test)]
fn channel(p: u32, shift: u32) -> i32 {
    ((p >> shift) & 0xff) as i32
}

/// `Select` (table 2, mode 11). The RFC's estimate is `L + T - TL`, so its
/// distances to L and T are |T - TL| and |L - TL| channel by channel.
#[inline(always)]
fn select(l: u32, t: u32, tl: u32) -> u32 {
    let (lb, tb, cb) = (l.to_le_bytes(), t.to_le_bytes(), tl.to_le_bytes());
    let mut pl = 0;
    let mut pt = 0;
    for c in 0..4 {
        pl += (i32::from(tb[c]) - i32::from(cb[c])).abs();
        pt += (i32::from(lb[c]) - i32::from(cb[c])).abs();
    }
    if pl < pt { l } else { t }
}

#[cfg(test)]
fn clamp255(v: i32) -> u32 {
    v.clamp(0, 255) as u32
}

/// `ClampAddSubtractFull(a, b, c)` per channel (mode 12).
#[inline(always)]
fn clamp_add_subtract_full(a: u32, b: u32, c: u32) -> u32 {
    let (a, b, c) = (a.to_le_bytes(), b.to_le_bytes(), c.to_le_bytes());
    u32::from_le_bytes(std::array::from_fn(|i| {
        (i16::from(a[i]) + i16::from(b[i]) - i16::from(c[i])).clamp(0, 255) as u8
    }))
}

/// `ClampAddSubtractHalf(a, b)` per channel (mode 13). The RFC's `/ 2` is
/// C's division, which truncates toward zero; so does Rust's.
#[inline(always)]
fn clamp_add_subtract_half(a: u32, b: u32) -> u32 {
    let (a, b) = (a.to_le_bytes(), b.to_le_bytes());
    u32::from_le_bytes(std::array::from_fn(|i| {
        let (x, y) = (i16::from(a[i]), i16::from(b[i]));
        (x + (x - y) / 2).clamp(0, 255) as u8
    }))
}

/// The predicted value for mode `mode` (table 2). Modes 14 and 15 — which
/// the green channel's low four bits can name but the RFC does not define
/// — predict as mode 0 (docs/PROVENANCE.md).
#[inline(always)]
pub(crate) fn predict(mode: u32, l: u32, t: u32, tl: u32, tr: u32) -> u32 {
    match mode {
        1 => l,
        2 => t,
        3 => tr,
        4 => tl,
        5 => average2(average2(l, tr), t),
        6 => average2(l, tl),
        7 => average2(l, t),
        8 => average2(tl, t),
        9 => average2(t, tr),
        10 => average2(average2(l, tl), average2(t, tr)),
        11 => select(l, t, tl),
        12 => clamp_add_subtract_full(l, t, tl),
        13 => clamp_add_subtract_half(average2(l, t), tl),
        _ => 0xff000000,
    }
}

/// The mode a predictor-image pixel names: its green channel. Only modes
/// 0..=13 exist; the low four bits are taken and 14, 15 fall to mode 0.
#[inline(always)]
pub(crate) fn mode_of(p: u32) -> u32 {
    (p >> 8) & 0xf
}

/// Undoes the predictor transform in place (section 3.5.1).
///
/// Row by row, with the row above as its own slice, and each block's run of
/// pixels by a loop compiled for its mode: the modes that do not read the
/// pixel to the left (T, TR, TL) have no dependence from pixel to pixel and
/// are vectorised.
pub(crate) fn inverse_predictor(
    px: &mut [u32],
    width: usize,
    height: usize,
    bits: u32,
    modes: &[u32],
) {
    if width == 0 || height == 0 {
        return;
    }
    let tw = subsample(width, bits);
    px[0] = add_pixels(px[0], 0xff000000);
    for x in 1..width {
        px[x] = add_pixels(px[x], px[x - 1]);
    }
    crate::simd::with_wide_vectors(|| {
        for y in 1..height {
            let (done, rest) = px.split_at_mut(y * width);
            let above = &done[(y - 1) * width..];
            let cur = &mut rest[..width];
            cur[0] = add_pixels(cur[0], above[0]);
            let mrow = (y >> bits) * tw;
            let mut x = 1;
            while x < width {
                let mode = mode_of(modes[mrow + (x >> bits)]);
                let end = (((x >> bits) + 1) << bits).min(width);
                // The last column's top-right neighbour is the row's own
                // first pixel (the RFC places it there); it is done apart.
                let run_end = end.min(width - 1);
                predict_run(mode, cur, above, x, run_end);
                if end == width && x < width {
                    let i = width - 1;
                    if run_end <= i {
                        let pred = predict(mode, cur[i - 1], above[i], above[i - 1], cur[0]);
                        cur[i] = add_pixels(cur[i], pred);
                    }
                }
                x = end;
            }
        }
    })
}

/// Pixels `x0..x1` of `cur` (none of them the last column, all past the
/// first) under one mode.
#[inline(always)]
fn predict_run(mode: u32, cur: &mut [u32], above: &[u32], x0: usize, x1: usize) {
    #[inline(always)]
    fn run<const M: u32>(cur: &mut [u32], above: &[u32], x0: usize, x1: usize) {
        if x0 >= x1 {
            return;
        }
        match M {
            // No dependence on the left: independent pixels.
            2..=4 => {
                let off = match M {
                    2 => 0,
                    3 => 1,
                    _ => -1isize,
                };
                let src = &above[(x0 as isize + off) as usize..(x1 as isize + off) as usize];
                for (c, &a) in cur[x0..x1].iter_mut().zip(src) {
                    *c = add_pixels(*c, a);
                }
            }
            _ => {
                let mut l = cur[x0 - 1];
                for i in x0..x1 {
                    let v = add_pixels(cur[i], predict(M, l, above[i], above[i - 1], above[i + 1]));
                    cur[i] = v;
                    l = v;
                }
            }
        }
    }
    match mode {
        1 => run::<1>(cur, above, x0, x1),
        2 => run::<2>(cur, above, x0, x1),
        3 => run::<3>(cur, above, x0, x1),
        4 => run::<4>(cur, above, x0, x1),
        5 => run::<5>(cur, above, x0, x1),
        6 => run::<6>(cur, above, x0, x1),
        7 => run::<7>(cur, above, x0, x1),
        8 => run::<8>(cur, above, x0, x1),
        9 => run::<9>(cur, above, x0, x1),
        10 => run::<10>(cur, above, x0, x1),
        11 => run::<11>(cur, above, x0, x1),
        12 => run::<12>(cur, above, x0, x1),
        13 => run::<13>(cur, above, x0, x1),
        _ => run::<0>(cur, above, x0, x1),
    }
}

/// Applies the predictor transform: `residual` receives each pixel minus
/// its prediction from `px` (section 3.5.1, as the encoder does it).
pub(crate) fn forward_predictor(
    px: &[u32],
    width: usize,
    height: usize,
    bits: u32,
    modes: &[u32],
    residual: &mut [u32],
) {
    let tw = subsample(width, bits);
    for y in 0..height {
        for x in 0..width {
            let i = y * width + x;
            let pred = predictor_at(
                px,
                width,
                x,
                y,
                mode_of(modes[(y >> bits) * tw + (x >> bits)]),
            );
            residual[i] = sub_pixels(px[i], pred);
        }
    }
}

/// The prediction for pixel (x, y) of `px` under `mode`, with the border
/// rules (the top-left pixel predicts black, the top row L, the left column
/// T).
#[inline(always)]
pub(crate) fn predictor_at(px: &[u32], width: usize, x: usize, y: usize, mode: u32) -> u32 {
    let i = y * width + x;
    if y == 0 {
        if x == 0 { 0xff000000 } else { px[i - 1] }
    } else if x == 0 {
        px[i - width]
    } else {
        predict(
            mode,
            px[i - 1],
            px[i - width],
            px[i - width - 1],
            px[i - width + 1],
        )
    }
}

/// A colour transform element (section 3.5.2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ColorTransformElement {
    pub(crate) green_to_red: i8,
    pub(crate) green_to_blue: i8,
    pub(crate) red_to_blue: i8,
}

impl ColorTransformElement {
    /// From a colour-transform-image pixel: red is red_to_blue, green
    /// green_to_blue, blue green_to_red.
    pub(crate) fn from_pixel(p: u32) -> Self {
        ColorTransformElement {
            green_to_red: p as u8 as i8,
            green_to_blue: (p >> 8) as u8 as i8,
            red_to_blue: (p >> 16) as u8 as i8,
        }
    }

    /// As a colour-transform-image pixel (alpha 255).
    pub(crate) fn to_pixel(self) -> u32 {
        0xff000000
            | (u32::from(self.red_to_blue as u8) << 16)
            | (u32::from(self.green_to_blue as u8) << 8)
            | u32::from(self.green_to_red as u8)
    }
}

/// `ColorTransformDelta`: a 3.5 fixed-point factor times a signed channel.
#[inline(always)]
pub(crate) fn delta(t: i8, c: i8) -> i32 {
    (i32::from(t) * i32::from(c)) >> 5
}

/// Undoes the colour transform in place (section 3.5.2), a block's run of
/// pixels at a time, vectorised.
pub(crate) fn inverse_color(
    px: &mut [u32],
    width: usize,
    height: usize,
    bits: u32,
    elements: &[u32],
) {
    let tw = subsample(width, bits);
    crate::simd::with_wide_vectors(|| {
        for (y, row) in px.chunks_exact_mut(width).take(height).enumerate() {
            let erow = &elements[(y >> bits) * tw..];
            for (run, &e) in row.chunks_mut(1 << bits).zip(erow) {
                let e = ColorTransformElement::from_pixel(e);
                for p in run {
                    *p = inverse_color_pixel(*p, e);
                }
            }
        }
    })
}

#[inline(always)]
pub(crate) fn inverse_color_pixel(p: u32, e: ColorTransformElement) -> u32 {
    let green = (p >> 8) as u8 as i8;
    let red = ((p >> 16) as i32 + delta(e.green_to_red, green)) & 0xff;
    let blue =
        ((p as i32 & 0xff) + delta(e.green_to_blue, green) + delta(e.red_to_blue, red as u8 as i8))
            & 0xff;
    (p & 0xff00ff00) | ((red as u32) << 16) | blue as u32
}

/// The colour transform of one pixel (the encoder's direction).
#[inline(always)]
pub(crate) fn forward_color_pixel(p: u32, e: ColorTransformElement) -> u32 {
    let green = (p >> 8) as u8 as i8;
    let red_byte = (p >> 16) as u8;
    let red = (i32::from(red_byte) - delta(e.green_to_red, green)) & 0xff;
    let blue =
        ((p as i32 & 0xff) - delta(e.green_to_blue, green) - delta(e.red_to_blue, red_byte as i8))
            & 0xff;
    (p & 0xff00ff00) | ((red as u32) << 16) | blue as u32
}

/// Undoes subtract-green in place (section 3.5.3).
pub(crate) fn add_green(px: &mut [u32]) {
    crate::simd::with_wide_vectors(|| {
        for p in px {
            let g = (*p >> 8) & 0xff;
            *p = add_pixels(*p, (g << 16) | g);
        }
    })
}

/// Subtract-green, the encoder's direction.
pub(crate) fn subtract_green(px: &mut [u32]) {
    for p in px {
        let g = (*p >> 8) & 0xff;
        *p = sub_pixels(*p, (g << 16) | g);
    }
}

/// How many bundled-pixel bits a colour table of `size` entries implies
/// (table 3).
pub(crate) fn bundle_bits(size: usize) -> u32 {
    match size {
        0..=2 => 3,
        3..=4 => 2,
        5..=16 => 1,
        _ => 0,
    }
}

/// Undoes colour indexing: `packed` is `subsample(width, bits)` wide; the
/// result is `width` wide (section 3.5.4). An index past the table gives
/// transparent black, as the RFC says.
pub(crate) fn inverse_color_indexing(
    packed: &[u32],
    width: usize,
    height: usize,
    bits: u32,
    table: &[u32],
) -> Vec<u32> {
    let pw = subsample(width, bits);
    let mut out = Vec::with_capacity(width * height);
    // Every value a byte can index, so no index needs a bounds check.
    let mut lut = [0u32; 256];
    lut[..table.len()].copy_from_slice(table);
    if bits == 0 {
        out.extend(
            packed[..width * height]
                .iter()
                .map(|&p| lut[((p >> 8) & 0xff) as usize]),
        );
        return out;
    }
    let per = 1usize << bits;
    let field = 8 >> bits;
    let mask = (1u32 << field) - 1;
    for y in 0..height {
        let row = &packed[y * pw..(y + 1) * pw];
        for x in 0..width {
            let g = (row[x >> bits] >> 8) & 0xff;
            let idx = (g >> ((x & (per - 1)) as u32 * field)) & mask;
            out.push(lut[idx as usize]);
        }
    }
    out
}

/// Colour indexing, the encoder's direction: `indices` (one per pixel, each
/// below the table size) bundled into green channels.
pub(crate) fn bundle(indices: &[u8], width: usize, height: usize, bits: u32) -> Vec<u32> {
    let pw = subsample(width, bits);
    let field = 8 >> bits;
    let mut out = vec![0xff000000u32; pw * height];
    for y in 0..height {
        for x in 0..width {
            let idx = u32::from(indices[y * width + x]);
            let o = &mut out[y * pw + (x >> bits)];
            *o |= idx << (8 + (x & ((1 << bits) - 1)) as u32 * field);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(n: usize, seed: u32) -> Vec<u32> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                s
            })
            .collect()
    }

    #[test]
    fn pixel_arithmetic() {
        let a = noise(1000, 7);
        let b = noise(1000, 9);
        for (&x, &y) in a.iter().zip(&b) {
            assert_eq!(sub_pixels(add_pixels(x, y), y), x);
            let avg = average2(x, y);
            for s in [0, 8, 16, 24] {
                assert_eq!(
                    (avg >> s) & 0xff,
                    (((x >> s) & 0xff) + ((y >> s) & 0xff)) / 2
                );
            }
        }
    }

    fn select_reference(l: u32, t: u32, tl: u32) -> u32 {
        let mut pl = 0;
        let mut pt = 0;
        for s in [24, 16, 8, 0] {
            let estimate = channel(l, s) + channel(t, s) - channel(tl, s);
            pl += (estimate - channel(l, s)).abs();
            pt += (estimate - channel(t, s)).abs();
        }
        if pl < pt { l } else { t }
    }

    #[test]
    fn channel_arithmetic_matches_the_definitions() {
        let n = noise(30_000, 77);
        for &[a, b, c] in n.as_chunks::<3>().0 {
            assert_eq!(select(a, b, c), select_reference(a, b, c));
            let mut full = 0;
            let mut half = 0;
            for s in [24, 16, 8, 0] {
                full |= clamp255(channel(a, s) + channel(b, s) - channel(c, s)) << s;
                let (x, y) = (channel(a, s), channel(b, s));
                half |= clamp255(x + (x - y) / 2) << s;
            }
            assert_eq!(clamp_add_subtract_full(a, b, c), full);
            assert_eq!(clamp_add_subtract_half(a, b), half);
        }
    }

    /// The predictor transform undone a pixel at a time, as first written.
    fn inverse_predictor_reference(
        px: &mut [u32],
        width: usize,
        height: usize,
        bits: u32,
        modes: &[u32],
    ) {
        let tw = subsample(width, bits);
        px[0] = add_pixels(px[0], 0xff000000);
        for x in 1..width {
            px[x] = add_pixels(px[x], px[x - 1]);
        }
        for y in 1..height {
            let row = y * width;
            px[row] = add_pixels(px[row], px[row - width]);
            for x in 1..width {
                let mode = mode_of(modes[(y >> bits) * tw + (x >> bits)]);
                let i = row + x;
                let pred = predict(
                    mode,
                    px[i - 1],
                    px[i - width],
                    px[i - width - 1],
                    px[i - width + 1],
                );
                px[i] = add_pixels(px[i], pred);
            }
        }
    }

    #[test]
    fn inverse_transforms_match_the_reference() {
        for (w, h, bits) in [
            (1, 1, 2),
            (1, 9, 2),
            (9, 1, 3),
            (2, 2, 2),
            (37, 23, 2),
            (64, 17, 3),
            (33, 33, 5),
            (100, 7, 9),
        ] {
            for seed in 1..6 {
                let px = noise(w * h, seed * 31);
                let tw = subsample(w, bits);
                // Every mode, 14 and 15 included, in every position.
                let modes: Vec<u32> = noise(tw * subsample(h, bits), seed)
                    .iter()
                    .map(|&r| (r & 0xffff_00ff) | ((r % 16) << 8))
                    .collect();
                let mut a = px.clone();
                let mut b = px.clone();
                inverse_predictor(&mut a, w, h, bits, &modes);
                inverse_predictor_reference(&mut b, w, h, bits, &modes);
                assert_eq!(a, b, "{w}x{h} bits {bits} seed {seed}");
                let elements = noise(tw * subsample(h, bits), seed + 99);
                let mut a = px.clone();
                inverse_color(&mut a, w, h, bits, &elements);
                let b: Vec<u32> = (0..w * h)
                    .map(|i| {
                        inverse_color_pixel(
                            px[i],
                            ColorTransformElement::from_pixel(
                                elements[((i / w) >> bits) * tw + ((i % w) >> bits)],
                            ),
                        )
                    })
                    .collect();
                assert_eq!(a, b, "colour {w}x{h} bits {bits} seed {seed}");
            }
        }
    }

    #[test]
    fn predictor_round_trips_every_mode() {
        let (w, h, bits) = (37, 23, 2);
        let px = noise(w * h, 3);
        let tw = subsample(w, bits);
        let modes: Vec<u32> = (0..tw * subsample(h, bits))
            .map(|i| 0xff000000 | (((i % 16) as u32) << 8))
            .collect();
        let mut res = vec![0; w * h];
        forward_predictor(&px, w, h, bits, &modes, &mut res);
        inverse_predictor(&mut res, w, h, bits, &modes);
        assert_eq!(res, px);
    }

    #[test]
    fn color_transform_round_trips() {
        for (k, &p) in noise(5000, 11).iter().enumerate() {
            let e = ColorTransformElement::from_pixel(noise(1, k as u32 + 1)[0]);
            assert_eq!(inverse_color_pixel(forward_color_pixel(p, e), e), p);
            assert_eq!(ColorTransformElement::from_pixel(e.to_pixel()), e);
        }
        let mut px = noise(100, 5);
        let orig = px.clone();
        subtract_green(&mut px);
        add_green(&mut px);
        assert_eq!(px, orig);
    }

    #[test]
    fn bundling_round_trips() {
        for size in [2usize, 3, 4, 5, 16, 17, 256] {
            let bits = bundle_bits(size);
            let (w, h) = (13, 5);
            let table: Vec<u32> = noise(size, 17);
            let idx: Vec<u8> = (0..w * h).map(|i| (i * 7 % size) as u8).collect();
            let packed = bundle(&idx, w, h, bits);
            let out = inverse_color_indexing(&packed, w, h, bits, &table);
            let want: Vec<u32> = idx.iter().map(|&i| table[i as usize]).collect();
            assert_eq!(out, want);
        }
    }
}
