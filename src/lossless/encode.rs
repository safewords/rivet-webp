//! The VP8L encoder: this crate's own design on RFC 9649 section 3's
//! bitstream.
//!
//! - **Strategies.** A picture of at most 256 colours is tried with colour
//!   indexing (the palette sorted, pixels bundled where the table allows);
//!   any picture with subtract-green, the predictor transform and the
//!   colour transform. At higher efforts both, and no transforms at all,
//!   are tried, and the smallest stream is kept.
//! - **Predictor modes** are chosen per block by the bits their residuals
//!   would cost under the statistics of the blocks already decided (an
//!   adaptive estimate: block by block, the running histogram of chosen
//!   residuals prices the next block's candidates).
//! - **Colour transform** elements are searched per block, coarse to fine,
//!   for the smallest Shannon entropy of the transformed red and blue.
//! - **Backward references** come from hash chains with lazy matching
//!   (`lz77`); the **colour cache** size is the one, 0 to 10 bits, whose
//!   symbol statistics cost least.
//! - **Meta prefix codes**: block histograms are clustered by merging
//!   randomly drawn pairs when the merge lowers the estimated total (as
//!   the RFC's rationale suggests), then each block moves to the cluster
//!   that codes it cheapest; the entropy image is used only when the
//!   estimate beats one code group.
//! - **Prefix codes** are length-limited Huffman codes (15 bits; 7 for the
//!   code length code), sent with the simple code length code for one or
//!   two small symbols and otherwise run-length coded with codes 16-18.
//!   `max_symbol` is never used (docs/PROVENANCE.md says why).

use super::histogram::{Histogram, nlog2n_table, shannon_bits};
use super::lz77::{CostModel, DistanceCoder, MatchParams, Token, apply_cache, backward_references};
use super::transform::{
    ColorTransformElement, bundle, bundle_bits, forward_color_pixel, mode_of, predictor_at,
    sub_pixels, subtract_green,
};
use super::*;
use crate::bits::BitWriter;
use crate::huffman::{codes_from_lengths, lengths_from_counts};

/// Encodes ARGB pixels as a complete VP8L bitstream (header included).
pub(crate) fn encode(argb: &[u32], width: usize, height: usize, effort: u8) -> Vec<u8> {
    debug_assert!((1..=16384).contains(&width) && (1..=16384).contains(&height));
    let alpha_used = argb.iter().any(|&p| p >> 24 != 0xff);
    let stream = best_stream(argb, width, height, effort);
    let mut out = Vec::with_capacity(stream.len() + 5);
    out.push(SIGNATURE);
    let bits = (width as u32 - 1) | ((height as u32 - 1) << 14) | (u32::from(alpha_used) << 28);
    out.extend_from_slice(&bits.to_le_bytes());
    out.extend_from_slice(&stream);
    out
}

/// Encodes ARGB pixels as a headerless image-stream (an `ALPH` payload).
/// `_alpha_plane` notes that only green varies, which the strategies
/// handle on their own.
pub(crate) fn encode_headerless(
    argb: &[u32],
    width: usize,
    height: usize,
    effort: u8,
    _alpha_plane: bool,
) -> Vec<u8> {
    best_stream(argb, width, height, effort)
}

/// Tries the strategies `effort` allows; keeps the smallest.
///
/// The strategies are independent, so they run on several threads; the
/// first of the smallest is kept, as in a serial run.
fn best_stream(px: &[u32], width: usize, height: usize, effort: u8) -> Vec<u8> {
    let small = width * height <= 4096;
    let palette = palette_of(px);
    #[derive(Clone, Copy)]
    enum Strategy {
        Palette,
        Spatial { color: bool },
        Plain,
    }
    let mut tried = Vec::new();
    if palette.is_some() {
        tried.push(Strategy::Palette);
    }
    if palette.is_none() || effort >= 3 || small {
        tried.push(Strategy::Spatial { color: true });
    }
    if effort >= 5 || small {
        tried.push(Strategy::Plain);
    }
    if effort >= 6 && palette.is_none() {
        tried.push(Strategy::Spatial { color: false });
    }
    let streams = crate::par::map(tried.len(), 0, |i| match tried[i] {
        Strategy::Palette => palette_stream(
            px,
            width,
            height,
            palette.as_deref().unwrap_or_default(),
            effort,
        ),
        Strategy::Spatial { color } => spatial_stream(px, width, height, effort, color),
        Strategy::Plain => plain_stream(px, width, height, effort),
    });
    let mut best: Option<Vec<u8>> = None;
    for s in streams {
        if best.as_ref().is_none_or(|b| s.len() < b.len()) {
            best = Some(s);
        }
    }
    best.unwrap_or_default()
}

/// The distinct colours, sorted, if there are at most 256.
fn palette_of(px: &[u32]) -> Option<Vec<u32>> {
    let mut seen: Vec<u32> = Vec::with_capacity(257);
    // A small open-addressed set: 257 colours is the most that matter.
    let mut table = [0u32; 1024];
    let mut used = [false; 1024];
    let mut last = None;
    for &p in px {
        if last == Some(p) {
            continue;
        }
        last = Some(p);
        let mut k = (p.wrapping_mul(0x9E37_79B1) >> 22) as usize;
        loop {
            if !used[k] {
                used[k] = true;
                table[k] = p;
                seen.push(p);
                if seen.len() > 256 {
                    return None;
                }
                break;
            }
            if table[k] == p {
                break;
            }
            k = (k + 1) & 1023;
        }
    }
    seen.sort_unstable();
    Some(seen)
}

/// Colour indexing: the table, then the bundled indices.
fn palette_stream(px: &[u32], width: usize, height: usize, palette: &[u32], effort: u8) -> Vec<u8> {
    let mut bw = BitWriter::new();
    bw.write(1, 1);
    bw.write(3, 2);
    bw.write(palette.len() as u32 - 1, 8);
    let mut deltas = Vec::with_capacity(palette.len());
    for (i, &c) in palette.iter().enumerate() {
        deltas.push(if i == 0 {
            c
        } else {
            sub_pixels(c, palette[i - 1])
        });
    }
    write_coded_image(&mut bw, &deltas, palette.len(), 1, effort, false);
    let index_of = |p: u32| palette.binary_search(&p).unwrap_or(0) as u8;
    let mut indices = Vec::with_capacity(px.len());
    let mut last = (u32::MAX, 0u8);
    for &p in px {
        if p != last.0 {
            last = (p, index_of(p));
        }
        indices.push(last.1);
    }
    let bits = bundle_bits(palette.len());
    let packed = bundle(&indices, width, height, bits);
    bw.write(0, 1);
    write_coded_image(
        &mut bw,
        &packed,
        subsample(width, bits),
        height,
        effort,
        true,
    );
    bw.finish()
}

/// No transforms.
fn plain_stream(px: &[u32], width: usize, height: usize, effort: u8) -> Vec<u8> {
    let mut bw = BitWriter::new();
    bw.write(0, 1);
    write_coded_image(&mut bw, px, width, height, effort, true);
    bw.finish()
}

/// Subtract-green, predictor, colour transform.
fn spatial_stream(px: &[u32], width: usize, height: usize, effort: u8, color: bool) -> Vec<u8> {
    let mut bw = BitWriter::new();
    let mut img = px.to_vec();
    bw.write(1, 1);
    bw.write(2, 2);
    subtract_green(&mut img);

    let pbits = if effort >= 3 { 2 } else { 3 };
    let modes = choose_predictors(&img, width, height, pbits, effort);
    bw.write(1, 1);
    bw.write(0, 2);
    bw.write(pbits - 2, 3);
    write_coded_image(
        &mut bw,
        &modes,
        subsample(width, pbits),
        subsample(height, pbits),
        effort,
        false,
    );
    let mut residual = vec![0u32; img.len()];
    super::transform::forward_predictor(&img, width, height, pbits, &modes, &mut residual);

    if color && effort >= 1 {
        let cbits = if width * height < 256 * 256 { 4 } else { 5 };
        let elements = choose_color_transform(&residual, width, height, cbits, effort);
        if elements.iter().any(|&e| e != 0xff000000) {
            bw.write(1, 1);
            bw.write(1, 2);
            bw.write(cbits - 2, 3);
            let tw = subsample(width, cbits);
            write_coded_image(
                &mut bw,
                &elements,
                tw,
                subsample(height, cbits),
                effort,
                false,
            );
            for y in 0..height {
                for x in 0..width {
                    let e = ColorTransformElement::from_pixel(
                        elements[(y >> cbits) * tw + (x >> cbits)],
                    );
                    let i = y * width + x;
                    residual[i] = forward_color_pixel(residual[i], e);
                }
            }
        }
    }
    bw.write(0, 1);
    write_coded_image(&mut bw, &residual, width, height, effort, true);
    bw.finish()
}

/// Bits a residual byte costs under counts `hist` (with a floor so an
/// unseen value is dear but finite).
fn cost_table(hist: &[[u32; 256]; 4]) -> [[f32; 256]; 4] {
    let mut t = [[0f32; 256]; 4];
    for c in 0..4 {
        let total: f64 = hist[c].iter().map(|&v| f64::from(v)).sum::<f64>() + 256.0 * 0.25;
        for v in 0..256 {
            t[c][v] = -((f64::from(hist[c][v]) + 0.25) / total).log2() as f32;
        }
    }
    t
}

/// Bits charged for a predictor mode neither neighbouring block uses.
const SWITCH_BITS: f32 = 14.0;

/// A predictor mode per block of `1 << bits` square.
fn choose_predictors(px: &[u32], width: usize, height: usize, bits: u32, effort: u8) -> Vec<u32> {
    let (tw, th) = (subsample(width, bits), subsample(height, bits));
    let candidates: Vec<u32> = if effort == 0 {
        vec![1, 2, 11]
    } else {
        (0..14).collect()
    };
    // A prior that small residuals (either sign) are likely.
    let mut hist = [[0u32; 256]; 4];
    for h in hist.iter_mut() {
        for (v, c) in h.iter_mut().enumerate() {
            let d = (v as i32).min(256 - v as i32);
            *c = (64 >> d.min(6)) as u32;
        }
    }
    let mut modes = vec![0xff000000u32; tw * th];
    let mut costs = cost_table(&hist);
    for by in 0..th {
        for bx in 0..tw {
            let (x0, y0) = (bx << bits, by << bits);
            let (x1, y1) = (
                ((bx + 1) << bits).min(width),
                ((by + 1) << bits).min(height),
            );
            let mut best = (f32::INFINITY, 1u32);
            // A mode the left or upper block already uses costs less to
            // signal in the predictor image.
            let left = if bx > 0 {
                mode_of(modes[by * tw + bx - 1])
            } else {
                u32::MAX
            };
            let up = if by > 0 {
                mode_of(modes[(by - 1) * tw + bx])
            } else {
                u32::MAX
            };
            for &m in &candidates {
                let start = if m == left || m == up {
                    0.0
                } else {
                    SWITCH_BITS
                };
                let area = (x0, x1, y0, y1);
                let cost = match m {
                    0 => block_cost::<0>(px, width, area, &costs, start, best.0),
                    1 => block_cost::<1>(px, width, area, &costs, start, best.0),
                    2 => block_cost::<2>(px, width, area, &costs, start, best.0),
                    3 => block_cost::<3>(px, width, area, &costs, start, best.0),
                    4 => block_cost::<4>(px, width, area, &costs, start, best.0),
                    5 => block_cost::<5>(px, width, area, &costs, start, best.0),
                    6 => block_cost::<6>(px, width, area, &costs, start, best.0),
                    7 => block_cost::<7>(px, width, area, &costs, start, best.0),
                    8 => block_cost::<8>(px, width, area, &costs, start, best.0),
                    9 => block_cost::<9>(px, width, area, &costs, start, best.0),
                    10 => block_cost::<10>(px, width, area, &costs, start, best.0),
                    11 => block_cost::<11>(px, width, area, &costs, start, best.0),
                    12 => block_cost::<12>(px, width, area, &costs, start, best.0),
                    _ => block_cost::<13>(px, width, area, &costs, start, best.0),
                };
                if cost < best.0 {
                    best = (cost, m);
                }
            }
            modes[by * tw + bx] = 0xff000000 | (best.1 << 8);
            for y in y0..y1 {
                for x in x0..x1 {
                    let r = sub_pixels(px[y * width + x], predictor_at(px, width, x, y, best.1));
                    hist[0][(r >> 24) as usize] += 1;
                    hist[1][((r >> 16) & 0xff) as usize] += 1;
                    hist[2][((r >> 8) & 0xff) as usize] += 1;
                    hist[3][(r & 0xff) as usize] += 1;
                }
            }
        }
        costs = cost_table(&hist);
    }
    modes
}

/// The bits a block's residuals cost under mode `M`, added in raster order
/// to `start`; once a row ends at `bound` or more, the rest is skipped (the
/// mode cannot win). One instance per mode, so the prediction is not
/// chosen per pixel.
#[inline(never)]
fn block_cost<const M: u32>(
    px: &[u32],
    width: usize,
    (x0, x1, y0, y1): (usize, usize, usize, usize),
    costs: &[[f32; 256]; 4],
    start: f32,
    bound: f32,
) -> f32 {
    let mut cost = start;
    for y in y0..y1 {
        for x in x0..x1 {
            let r = sub_pixels(px[y * width + x], predictor_at(px, width, x, y, M));
            cost += costs[0][(r >> 24) as usize]
                + costs[1][((r >> 16) & 0xff) as usize]
                + costs[2][((r >> 8) & 0xff) as usize]
                + costs[3][(r & 0xff) as usize];
        }
        if cost >= bound {
            break;
        }
    }
    cost
}

/// Colour transform elements per block, each searched for the least
/// entropy of the block's transformed red and blue.
fn choose_color_transform(
    px: &[u32],
    width: usize,
    height: usize,
    bits: u32,
    effort: u8,
) -> Vec<u32> {
    let (tw, th) = (subsample(width, bits), subsample(height, bits));
    let nlog = nlog2n_table(1 << (2 * bits));
    // Every other pixel at the lower efforts.
    let step = if effort >= 5 { 1 } else { 2 };
    let mut out = vec![0xff000000u32; tw * th];
    let mut prev = ColorTransformElement::default();
    let (mut r, mut g, mut b) = (Vec::new(), Vec::new(), Vec::new());
    for by in 0..th {
        for bx in 0..tw {
            r.clear();
            g.clear();
            b.clear();
            let (x0, y0) = (bx << bits, by << bits);
            let (x1, y1) = (
                ((bx + 1) << bits).min(width),
                ((by + 1) << bits).min(height),
            );
            let mut k = 0;
            for y in y0..y1 {
                for x in x0..x1 {
                    k += 1;
                    if k % step != 0 {
                        continue;
                    }
                    let p = px[y * width + x];
                    r.push((p >> 16) as u8);
                    g.push((p >> 8) as u8 as i8);
                    b.push(p as u8);
                }
            }
            if r.is_empty() {
                continue;
            }
            let n = r.len() as u32;
            // A switch from the previous block's value costs a little: the
            // transform image then codes better.
            let red_cost = |t: i8| -> f64 {
                let mut h = [0u32; 256];
                for i in 0..r.len() {
                    h[(i32::from(r[i]) - super::transform::delta(t, g[i])) as u8 as usize] += 1;
                }
                shannon_bits(&h, n, &nlog) + if t == prev.green_to_red { 0.0 } else { 6.0 }
            };
            let blue_cost = |gb: i8, rb: i8| -> f64 {
                let mut h = [0u32; 256];
                for i in 0..r.len() {
                    let v = i32::from(b[i])
                        - super::transform::delta(gb, g[i])
                        - super::transform::delta(rb, r[i] as i8);
                    h[v as u8 as usize] += 1;
                }
                shannon_bits(&h, n, &nlog)
                    + if gb == prev.green_to_blue && rb == prev.red_to_blue {
                        0.0
                    } else {
                        6.0
                    }
            };
            let g2r = search(red_cost, effort, prev.green_to_red);
            let mut g2b = search(|t| blue_cost(t, 0), effort, prev.green_to_blue);
            let r2b = search(|t| blue_cost(g2b, t), effort, prev.red_to_blue);
            if effort >= 4 {
                g2b = search(|t| blue_cost(t, r2b), effort, g2b);
            }
            let e = ColorTransformElement {
                green_to_red: g2r,
                green_to_blue: g2b,
                red_to_blue: r2b,
            };
            out[by * tw + bx] = e.to_pixel();
            prev = e;
        }
    }
    out
}

/// The `i8` minimising `cost`: a coarse grid, then halving steps around the
/// best, with `hint` (the neighbour's value) and 0 always considered.
fn search(cost: impl Fn(i8) -> f64, effort: u8, hint: i8) -> i8 {
    let mut best = (cost(0), 0i8);
    let try_ = |t: i8, best: &mut (f64, i8)| {
        let c = cost(t);
        if c < best.0 {
            *best = (c, t);
        }
    };
    if hint != 0 {
        try_(hint, &mut best);
    }
    let coarse: i32 = if effort >= 5 { 8 } else { 16 };
    let mut t = -128i32;
    while t <= 127 {
        if t != 0 {
            try_(t as i8, &mut best);
        }
        t += coarse;
    }
    let mut s = coarse / 2;
    while s >= 1 {
        let centre = i32::from(best.1);
        for d in [-s, s] {
            let v = centre + d;
            if (-128..=127).contains(&v) {
                try_(v as i8, &mut best);
            }
        }
        s /= 2;
    }
    best.1
}

/// The colour cache size (bits) whose statistics cost least.
fn choose_cache_bits(tokens: &[Token], px: &[u32], effort: u8) -> u32 {
    if effort == 0 {
        return 0;
    }
    let mut best = (f64::INFINITY, 0u32);
    let candidates: Vec<u32> = if effort >= 3 {
        (0..=10).collect()
    } else {
        vec![0, 4, 7, 10]
    };
    for bits in candidates {
        let mut h = Histogram::new(bits);
        if bits == 0 {
            for t in tokens {
                h.add(t);
            }
        } else {
            let mut cache = vec![0u32; 1 << bits];
            let mut pos = 0;
            for t in tokens {
                match *t {
                    Token::Copy { len, .. } => {
                        for &p in &px[pos..pos + len as usize] {
                            cache[cache_index(p, bits)] = p;
                        }
                        h.add(t);
                        pos += len as usize;
                    }
                    _ => {
                        let p = px[pos];
                        let k = cache_index(p, bits);
                        if cache[k] == p {
                            h.add(&Token::Cache(k as u32));
                        } else {
                            cache[k] = p;
                            h.add(&Token::Literal(p));
                        }
                        pos += 1;
                    }
                }
            }
        }
        let c = h.cost();
        if c < best.0 {
            best = (c, bits);
        }
    }
    best.1
}

/// The prefix codes of one alphabet as the writer uses them.
struct Code {
    codes: Vec<u16>,
    /// Bits per symbol: 0 for every symbol of a one-symbol code.
    bits: Vec<u8>,
}

/// `entropy-coded-image` (or, with `level0`, `spatially-coded-image`):
/// colour cache info, meta prefix codes, prefix codes, the data.
fn write_coded_image(
    bw: &mut BitWriter,
    px: &[u32],
    width: usize,
    height: usize,
    effort: u8,
    level0: bool,
) {
    let coder = DistanceCoder::new(width);
    let params = MatchParams::for_effort(effort);
    let mut tokens = backward_references(px, width, &params, &coder, None);
    let copied: usize = tokens
        .iter()
        .map(|t| {
            if let Token::Copy { len, .. } = t {
                *len as usize
            } else {
                0
            }
        })
        .sum();
    // A second pass priced by the first one's statistics, where copies are
    // common enough for their pricing to matter.
    if effort >= 3 && px.len() > 64 && (effort >= 5 || copied * 20 >= px.len()) {
        let stats = |t: &[Token]| {
            let mut h = Histogram::new(0);
            for x in t {
                h.add(x);
            }
            h
        };
        let first = stats(&tokens);
        let model = CostModel::new(px, &first.codes.clone().map(|c| bit_costs(&c)));
        let second = backward_references(px, width, &params, &coder, Some(&model));
        if stats(&second).cost() < first.cost() {
            tokens = second;
        }
    }
    let cache_bits = choose_cache_bits(&tokens, px, effort);
    let tokens = apply_cache(&tokens, px, cache_bits);
    if cache_bits > 0 {
        bw.write(1, 1);
        bw.write(cache_bits, 4);
    } else {
        bw.write(0, 1);
    }
    let clusters = if level0 {
        cluster(&tokens, width, height, cache_bits, effort)
    } else {
        None
    };
    let (block_bits, block_group, histograms) = match clusters {
        Some((bits, map, hists)) => {
            bw.write(1, 1);
            bw.write(bits - 2, 3);
            let (ew, eh) = (subsample(width, bits), subsample(height, bits));
            let image: Vec<u32> = map
                .iter()
                .map(|&g| 0xff000000 | ((g >> 8) << 16) | ((g & 0xff) << 8))
                .collect();
            write_coded_image(bw, &image, ew, eh, effort, false);
            (bits, map, hists)
        }
        None => {
            if level0 {
                bw.write(0, 1);
            }
            let mut h = Histogram::new(cache_bits);
            for t in &tokens {
                h.add(t);
            }
            (0, Vec::new(), vec![h])
        }
    };
    let groups: Vec<[Code; 5]> = histograms
        .iter()
        .map(|h| h.codes.clone().map(|c| write_code(bw, &c)))
        .collect();
    let ew = if block_group.is_empty() {
        0
    } else {
        subsample(width, block_bits)
    };
    let (mut x, mut y) = (0usize, 0usize);
    for t in &tokens {
        let g = if block_group.is_empty() {
            &groups[0]
        } else {
            &groups[block_group[(y >> block_bits) * ew + (x >> block_bits)] as usize]
        };
        match *t {
            Token::Literal(p) => {
                let (gr, r, b, a) = (
                    ((p >> 8) & 0xff) as usize,
                    ((p >> 16) & 0xff) as usize,
                    (p & 0xff) as usize,
                    (p >> 24) as usize,
                );
                bw.write(u32::from(g[0].codes[gr]), u32::from(g[0].bits[gr]));
                bw.write(u32::from(g[1].codes[r]), u32::from(g[1].bits[r]));
                bw.write(u32::from(g[2].codes[b]), u32::from(g[2].bits[b]));
                bw.write(u32::from(g[3].codes[a]), u32::from(g[3].bits[a]));
            }
            Token::Cache(i) => {
                let s = NUM_LITERALS + NUM_LENGTH_CODES + i as usize;
                bw.write(u32::from(g[0].codes[s]), u32::from(g[0].bits[s]));
            }
            Token::Copy { len, dist_code } => {
                let (lc, lb, le) = prefix_encode(len as usize);
                let s = NUM_LITERALS + lc;
                bw.write(u32::from(g[0].codes[s]), u32::from(g[0].bits[s]));
                bw.write(le, lb);
                let (dc, db, de) = prefix_encode(dist_code as usize);
                bw.write(u32::from(g[4].codes[dc]), u32::from(g[4].bits[dc]));
                bw.write(de, db);
            }
        }
        x += t.pixels();
        while x >= width {
            x -= width;
            y += 1;
        }
    }
}

/// Writes one prefix code for symbol `counts` and returns it.
fn write_code(bw: &mut BitWriter, counts: &[u32]) -> Code {
    let used: Vec<usize> = (0..counts.len()).filter(|&s| counts[s] > 0).collect();
    let mut bits = vec![0u8; counts.len()];
    let mut codes = vec![0u16; counts.len()];
    if used.len() <= 2 && used.iter().all(|&s| s < 256) {
        // Simple code length code.
        bw.write(1, 1);
        let s0 = used.first().copied().unwrap_or(0);
        bw.write(used.len().saturating_sub(1) as u32, 1);
        if s0 < 2 {
            bw.write(0, 1);
            bw.write(s0 as u32, 1);
        } else {
            bw.write(1, 1);
            bw.write(s0 as u32, 8);
        }
        if used.len() == 2 {
            bw.write(used[1] as u32, 8);
            bits[used[0]] = 1;
            bits[used[1]] = 1;
            codes[used[1]] = 1;
        }
        return Code { codes, bits };
    }
    let lengths = lengths_from_counts(counts, crate::huffman::MAX_LENGTH);
    write_lengths(bw, &lengths);
    if used.len() > 1 {
        codes = codes_from_lengths(&lengths);
        bits = lengths;
    }
    Code { codes, bits }
}

/// The normal code length code: lengths run-length coded with 16 (repeat
/// the previous non-zero length), 17 and 18 (runs of zeros), then coded
/// with a code of their own.
fn write_lengths(bw: &mut BitWriter, lengths: &[u8]) {
    // (symbol, extra bits, extra value)
    let mut tokens: Vec<(u8, u8, u8)> = Vec::new();
    let mut prev = 8u8;
    let mut i = 0;
    while i < lengths.len() {
        let v = lengths[i];
        let mut run = lengths[i..].iter().take_while(|&&l| l == v).count();
        i += run;
        if v == 0 {
            while run >= 11 {
                let r = run.min(138);
                tokens.push((18, 7, (r - 11) as u8));
                run -= r;
            }
            if run >= 3 {
                tokens.push((17, 3, (run - 3) as u8));
                run = 0;
            }
            for _ in 0..run {
                tokens.push((0, 0, 0));
            }
        } else {
            if v != prev {
                tokens.push((v, 0, 0));
                run -= 1;
                prev = v;
            }
            while run >= 3 {
                let r = run.min(6);
                tokens.push((16, 2, (r - 3) as u8));
                run -= r;
            }
            for _ in 0..run {
                tokens.push((v, 0, 0));
            }
        }
    }
    let mut counts = [0u32; 19];
    for t in &tokens {
        counts[t.0 as usize] += 1;
    }
    let cl_lengths = lengths_from_counts(&counts, 7);
    let cl_codes = codes_from_lengths(&cl_lengths);
    let single = cl_lengths.iter().filter(|&&l| l > 0).count() == 1;
    bw.write(0, 1);
    let num = CODE_LENGTH_ORDER
        .iter()
        .rposition(|&s| cl_lengths[s] != 0)
        .map_or(4, |p| (p + 1).max(4));
    bw.write(num as u32 - 4, 4);
    for &s in &CODE_LENGTH_ORDER[..num] {
        bw.write(u32::from(cl_lengths[s]), 3);
    }
    bw.write(0, 1);
    for &(s, nb, e) in &tokens {
        if !single {
            bw.write(
                u32::from(cl_codes[s as usize]),
                u32::from(cl_lengths[s as usize]),
            );
        }
        bw.write(u32::from(e), u32::from(nb));
    }
}

/// A small deterministic generator for the clustering's random pairs.
struct XorShift(u64);

impl XorShift {
    fn next(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }
}

/// Meta prefix codes: (block bits, group per block, group histograms), or
/// `None` when one group codes the image as cheaply.
fn cluster(
    tokens: &[Token],
    width: usize,
    height: usize,
    cache_bits: u32,
    effort: u8,
) -> Option<(u32, Vec<u32>, Vec<Histogram>)> {
    if effort < 3 || width * height < 64 * 64 {
        return None;
    }
    let max_blocks = if effort >= 5 { 2000 } else { 800 };
    let mut bits = 2;
    while bits < 9 && subsample(width, bits) * subsample(height, bits) > max_blocks {
        bits += 1;
    }
    bits = bits.max(4);
    let (bw_, bh) = (subsample(width, bits), subsample(height, bits));
    let nblocks = bw_ * bh;
    let mut blocks: Vec<Histogram> = (0..nblocks).map(|_| Histogram::new(cache_bits)).collect();
    let mut whole = Histogram::new(cache_bits);
    let (mut x, mut y) = (0usize, 0usize);
    for t in tokens {
        blocks[(y >> bits) * bw_ + (x >> bits)].add(t);
        whole.add(t);
        x += t.pixels();
        while x >= width {
            x -= width;
            y += 1;
        }
    }
    let single_cost = whole.cost();

    // One cluster per block that has tokens.
    let mut assign: Vec<usize> = vec![usize::MAX; nblocks];
    let mut clusters: Vec<Option<(Histogram, f64)>> = Vec::new();
    for (i, b) in blocks.iter().enumerate() {
        if b.codes[0].iter().any(|&c| c > 0) {
            assign[i] = clusters.len();
            clusters.push(Some((b.clone(), b.cost())));
        }
    }
    let mut alive: Vec<usize> = (0..clusters.len()).collect();
    let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);
    let tries = if effort >= 5 { 32 } else { 16 };
    let max_fails = if effort >= 5 { 12 } else { 6 };
    let mut fails = 0;
    while alive.len() > 1 && fails < max_fails {
        let mut best: Option<(f64, usize, usize)> = None;
        for _ in 0..tries {
            let i = rng.next(alive.len());
            let mut j = rng.next(alive.len() - 1);
            if j >= i {
                j += 1;
            }
            let (a, b) = (alive[i], alive[j]);
            let (ha, ca) = clusters[a].as_ref().unwrap();
            let (hb, cb) = clusters[b].as_ref().unwrap();
            let delta = ha.merged_cost(hb) - ca - cb;
            if best.is_none_or(|(d, _, _)| delta < d) {
                best = Some((delta, i, j));
            }
        }
        match best {
            Some((d, i, j)) if d < 0.0 => {
                let (a, b) = (alive[i], alive[j]);
                let (hb, _) = clusters[b].take().unwrap();
                let (ha, ca) = clusters[a].as_mut().unwrap();
                ha.merge(&hb);
                *ca = ha.cost();
                for g in assign.iter_mut() {
                    if *g == b {
                        *g = a;
                    }
                }
                alive.swap_remove(j);
                fails = 0;
            }
            _ => fails += 1,
        }
    }

    // Refinement: each block to the cluster that codes it cheapest.
    let costs: Vec<(usize, [Vec<f32>; 5])> = alive
        .iter()
        .map(|&c| {
            let h = &clusters[c].as_ref().unwrap().0;
            (c, h.codes.clone().map(|counts| bit_costs(&counts)))
        })
        .collect();
    for (bi, b) in blocks.iter().enumerate() {
        if assign[bi] == usize::MAX {
            continue;
        }
        let mut best = (f32::INFINITY, assign[bi]);
        for (c, cost) in &costs {
            let mut bitsum = 0f32;
            for k in 0..5 {
                for (s, &n) in b.codes[k].iter().enumerate() {
                    if n > 0 {
                        bitsum += n as f32 * cost[k][s];
                    }
                }
            }
            if bitsum < best.0 {
                best = (bitsum, *c);
            }
        }
        assign[bi] = best.1;
    }
    // Rebuild the histograms; number the clusters densely. Blocks without
    // tokens (covered by a copy from elsewhere) take group 0.
    let mut number = vec![u32::MAX; clusters.len()];
    let mut hists: Vec<Histogram> = Vec::new();
    let mut map = vec![0u32; nblocks];
    for bi in 0..nblocks {
        let c = assign[bi];
        if c == usize::MAX {
            continue;
        }
        if number[c] == u32::MAX {
            number[c] = hists.len() as u32;
            hists.push(Histogram::new(cache_bits));
        }
        map[bi] = number[c];
        hists[number[c] as usize].merge(&blocks[bi]);
    }
    if hists.len() <= 1 {
        return None;
    }
    let groups_cost: f64 = hists.iter().map(Histogram::cost).sum();
    // The entropy image: a few bits a block, more with more groups.
    let image_cost = nblocks as f64 * (hists.len() as f64).log2().max(1.0) * 0.6 + 60.0;
    if groups_cost + image_cost < single_cost {
        Some((bits, map, hists))
    } else {
        None
    }
}

/// Estimated bits per symbol under `counts` (unseen symbols dear).
fn bit_costs(counts: &[u32]) -> Vec<f32> {
    let total: f64 = counts.iter().map(|&c| f64::from(c)).sum();
    if total == 0.0 {
        return vec![20.0; counts.len()];
    }
    counts
        .iter()
        .map(|&c| {
            if c == 0 {
                20.0
            } else {
                (-(f64::from(c) / total).log2()) as f32
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::decode;
    use super::*;

    fn round_trip(px: &[u32], w: usize, h: usize) {
        for effort in [0u8, 2, 4, 6] {
            let data = encode(px, w, h, effort);
            let (hdr, out) = decode::decode(&data, u64::MAX).unwrap();
            assert_eq!((hdr.width as usize, hdr.height as usize), (w, h));
            assert!(out == px, "effort {effort}, {w}x{h}");
            let raw = decode::decode_headerless(&encode_headerless(px, w, h, effort, false), w, h)
                .unwrap();
            assert!(raw == px);
        }
    }

    fn noise(n: usize, seed: u64) -> Vec<u32> {
        let mut r = XorShift(seed);
        (0..n).map(|_| r.next(1 << 32) as u32).collect()
    }

    #[test]
    fn tiny_images() {
        round_trip(&[0x12345678], 1, 1);
        round_trip(&[0, 0xffffffff], 2, 1);
        round_trip(&[0, 0xffffffff, 7], 1, 3);
        round_trip(&noise(13, 3), 13, 1);
        round_trip(&noise(13, 4), 1, 13);
    }

    #[test]
    fn noise_and_gradients() {
        let (w, h) = (67, 45);
        round_trip(&noise(w * h, 9), w, h);
        let grad: Vec<u32> = (0..w * h)
            .map(|i| {
                0xff000000
                    | (((i % w) as u32 * 3) << 16)
                    | (((i / w) as u32 * 5) << 8)
                    | ((i % 7) as u32)
            })
            .collect();
        round_trip(&grad, w, h);
    }

    #[test]
    fn few_colours_bundle() {
        for n in [1u32, 2, 3, 4, 5, 16, 17, 200, 256, 257] {
            let (w, h) = (31, 17);
            let px: Vec<u32> = (0..w * h)
                .map(|i| {
                    ((((i as u32).wrapping_mul(2654435761)) >> 7) % n).wrapping_mul(0x01030507)
                })
                .collect();
            round_trip(&px, w, h);
        }
    }

    #[test]
    fn big_enough_for_meta_codes() {
        let (w, h) = (300, 200);
        let mut r = XorShift(77);
        let px: Vec<u32> = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                if x < 150 {
                    0xff000000 | ((x as u32 & 0xf0) << 16) | ((y as u32) << 8)
                } else if y < 100 {
                    r.next(1 << 32) as u32 | 0xff000000
                } else {
                    0x80000000 | (r.next(4) as u32 * 0x111111)
                }
            })
            .collect();
        round_trip(&px, w, h);
    }
}
