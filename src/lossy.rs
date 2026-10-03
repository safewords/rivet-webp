//! Lossy frames: the `VP8 ` chunk through rivet-vp8, and the conversion
//! between its 4:2:0 Y'CbCr and RGB.
//!
//! RFC 9649 section 2.5 says Recommendation 601 SHOULD be used and leaves
//! the details (range, chroma siting, upsampling, rounding) to the
//! application. This crate uses BT.601's studio range (Y' 16-235, Cb/Cr
//! 16-240), the matrix with Kr = 0.299 and Kb = 0.114, chroma samples sited
//! midway between luma samples, and bilinear chroma upsampling (weights 9,
//! 3, 3, 1 sixteenths) when decoding; 2x2 averaging when encoding. The
//! arithmetic is 14-bit (decode) and 16-bit (encode) fixed point, with
//! coefficients rounded from the BT.601 formulas.

use crate::error::{Result, bitstream};

/// Decodes a `VP8 ` payload into `rgba` (`width * height * 4` bytes; the
/// alpha bytes are set to 255).
pub(crate) fn decode(data: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    let mut dec = vp8::Decoder::new();
    let frame = dec.decode(data)?.ok_or_else(|| bitstream("VP8 frame marked not to be shown"))?;
    if (frame.width, frame.height) != (width, height) {
        return Err(bitstream(format!(
            "VP8 frame decoded at {}x{}, expected {width}x{height}",
            frame.width, frame.height
        )));
    }
    Ok(yuv_to_rgba(&frame))
}

// BT.601 studio range to RGB, times 2^14: R = 1.164384 (Y - 16) + 1.596027
// (Cr - 128); G = 1.164384 (Y - 16) - 0.391762 (Cb - 128) - 0.812968 (Cr -
// 128); B = 1.164384 (Y - 16) + 2.017232 (Cb - 128).
const KY: i32 = 19077;
const KRV: i32 = 26149;
const KGU: i32 = 6419;
const KGV: i32 = 13320;
const KBU: i32 = 33050;

#[inline(always)]
fn clamp8(v: i32) -> u8 {
    (v >> 14).clamp(0, 255) as u8
}

/// One pixel from Y' and from Cb, Cr times 16 (the upsampler's
/// precision).
#[inline(always)]
fn rgb(y: u8, u16x: i32, v16x: i32) -> [u8; 3] {
    let c = KY * (i32::from(y) - 16) + (1 << 13);
    // Chroma arrives times 16; the products are scaled back by 16 with
    // rounding before they join the luma term.
    let d = u16x - 128 * 16;
    let e = v16x - 128 * 16;
    [
        clamp8(c + ((KRV * e + 8) >> 4)),
        clamp8(c - ((KGU * d + KGV * e + 8) >> 4)),
        clamp8(c + ((KBU * d + 8) >> 4)),
    ]
}

/// The decoded frame as RGBA, chroma upsampled bilinearly.
pub(crate) fn yuv_to_rgba(f: &vp8::Frame) -> Vec<u8> {
    let (w, h) = (f.width as usize, f.height as usize);
    let (cw, ch) = (f.planes[1].width as usize, f.planes[1].height as usize);
    let (yp, up, vp) = (f.plane(0), f.plane(1), f.plane(2));
    let mut out = vec![255u8; w * h * 4];
    // Each chroma row interpolated vertically, times 4.
    let mut urow = vec![0i32; cw];
    let mut vrow = vec![0i32; cw];
    for y in 0..h {
        let near = y >> 1;
        let far = if y & 1 == 0 { near.saturating_sub(1) } else { (near + 1).min(ch - 1) };
        for i in 0..cw {
            urow[i] = 3 * i32::from(up[near * cw + i]) + i32::from(up[far * cw + i]);
            vrow[i] = 3 * i32::from(vp[near * cw + i]) + i32::from(vp[far * cw + i]);
        }
        let yr = &yp[y * w..(y + 1) * w];
        let orow = &mut out[y * w * 4..(y + 1) * w * 4];
        for x in 0..w {
            let n = x >> 1;
            let fx = if x & 1 == 0 { n.saturating_sub(1) } else { (n + 1).min(cw - 1) };
            let u = 3 * urow[n] + urow[fx];
            let v = 3 * vrow[n] + vrow[fx];
            let [r, g, b] = rgb(yr[x], u, v);
            orow[x * 4] = r;
            orow[x * 4 + 1] = g;
            orow[x * 4 + 2] = b;
        }
    }
    out
}

// RGB to BT.601 studio range, times 2^16.
const YR: i32 = 16829;
const YG: i32 = 33039;
const YB: i32 = 6416;
const UR: i32 = -9714;
const UG: i32 = -19071;
const UB: i32 = 28784;
const VR: i32 = 28784;
const VG: i32 = -24103;
const VB: i32 = -4681;

/// RGBA to a 4:2:0 frame; alpha is ignored here (it travels in ALPH).
pub(crate) fn rgba_to_yuv(rgba: &[u8], width: u32, height: u32) -> Result<vp8::Frame> {
    let (w, h) = (width as usize, height as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let mut y = vec![0u8; w * h];
    let mut u = vec![0u8; cw * ch];
    let mut v = vec![0u8; cw * ch];
    for j in 0..h {
        for i in 0..w {
            let p = &rgba[(j * w + i) * 4..];
            let (r, g, b) = (i32::from(p[0]), i32::from(p[1]), i32::from(p[2]));
            y[j * w + i] = ((YR * r + YG * g + YB * b + (16 << 16) + (1 << 15)) >> 16) as u8;
        }
    }
    for cj in 0..ch {
        for ci in 0..cw {
            let (mut r, mut g, mut b, mut n) = (0i32, 0i32, 0i32, 0i32);
            for j in 2 * cj..(2 * cj + 2).min(h) {
                for i in 2 * ci..(2 * ci + 2).min(w) {
                    let p = &rgba[(j * w + i) * 4..];
                    r += i32::from(p[0]);
                    g += i32::from(p[1]);
                    b += i32::from(p[2]);
                    n += 1;
                }
            }
            let round = |s: i32| -> u8 {
                let den = n << 16;
                let num = s + (128 * den);
                (((num + den / 2).div_euclid(den)).clamp(0, 255)) as u8
            };
            u[cj * cw + ci] = round(UR * r + UG * g + UB * b);
            v[cj * cw + ci] = round(VR * r + VG * g + VB * b);
        }
    }
    Ok(vp8::Frame::from_planes(width, height, &y, &u, &v)?)
}

/// The VP8 quantiser index (0 finest .. 127 coarsest) for a quality of
/// 1..=100: `127 * (1 - q/100)^0.85`, rounded. This crate's own curve; it
/// puts quality 80 at index 32 and 50 at index 70.
pub(crate) fn quantizer_for(quality: u8) -> u8 {
    let q = f64::from(quality.clamp(1, 100)) / 100.0;
    (127.0 * (1.0 - q).powf(0.85)).round().clamp(0.0, 127.0) as u8
}

/// Encodes RGBA as a `VP8 ` payload (a key frame).
pub(crate) fn encode(rgba: &[u8], width: u32, height: u32, quality: u8) -> Result<Vec<u8>> {
    let frame = rgba_to_yuv(rgba, width, height)?;
    let mut enc = vp8::Encoder::new(vp8::Config {
        width,
        height,
        quantizer: quantizer_for(quality),
        keyframe_interval: 1,
        ..Default::default()
    })?;
    Ok(enc.encode(&frame)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grey_and_primaries_survive_the_matrices() {
        for (r, g, b) in [(0u8, 0u8, 0u8), (255, 255, 255), (128, 128, 128), (255, 0, 0), (0, 255, 0), (0, 0, 255), (12, 200, 99)] {
            let rgba: Vec<u8> = (0..4).flat_map(|_| [r, g, b, 255]).collect();
            let f = rgba_to_yuv(&rgba, 2, 2).unwrap();
            let back = yuv_to_rgba(&f);
            for k in 0..3 {
                let want = [r, g, b][k];
                assert!((i32::from(back[k]) - i32::from(want)).abs() <= 2, "{:?} -> {:?}", (r, g, b), &back[..4]);
            }
        }
    }

    #[test]
    fn quality_curve() {
        assert_eq!(quantizer_for(100), 0);
        assert_eq!(quantizer_for(80), 32);
        assert!(quantizer_for(1) >= 125);
    }
}
