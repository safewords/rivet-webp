//! The encoder's backward references (RFC 9649 section 3.6.2.2) and colour
//! cache (3.6.2.3): pixels to a stream of literals, copies and cache hits.
//!
//! Matches are found through hash chains over pairs of pixels, plus two
//! candidates checked at every position whatever the chains hold: the pixel
//! to the left (runs) and the pixel above (repeated rows), whose distance
//! codes are the cheapest there are. Matching is greedy with one step of
//! lazy evaluation. Distances that land on one of the 120 neighbourhood
//! offsets are sent as their short codes.

use super::{MAX_DISTANCE_CODE, MAX_LENGTH, PLANE_CODES, cache_index, code_to_distance};

/// One coded element.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Token {
    /// A pixel coded channel by channel.
    Literal(u32),
    /// A colour cache index.
    Cache(u32),
    /// Copy `len` pixels from `dist_code` (already distance-mapped) back.
    Copy { len: u32, dist_code: u32 },
}

impl Token {
    /// Pixels the token produces.
    #[inline]
    pub(crate) fn pixels(&self) -> usize {
        match self {
            Token::Copy { len, .. } => *len as usize,
            _ => 1,
        }
    }
}

const HASH_BITS: u32 = 18;
/// The largest scan-line distance a code can carry.
const MAX_DISTANCE: usize = MAX_DISTANCE_CODE - PLANE_CODES;

/// Search depth by effort.
pub(crate) struct MatchParams {
    /// Hash chain steps per position (0: only the left and above
    /// candidates).
    pub(crate) chain: usize,
    /// Try the next position before taking a match shorter than this.
    pub(crate) lazy_below: usize,
    /// Stop searching a chain once a match this long is found.
    pub(crate) nice: usize,
}

impl MatchParams {
    pub(crate) fn for_effort(effort: u8) -> Self {
        let chain = match effort {
            0 => 0,
            1 => 8,
            2 => 16,
            3 => 32,
            4 => 64,
            5 => 160,
            _ => 400,
        };
        MatchParams {
            chain,
            lazy_below: if effort == 0 { 0 } else { 64 },
            nice: match effort {
                0..=3 => 64,
                4 => 128,
                5 => 512,
                _ => MAX_LENGTH,
            },
        }
    }
}

/// Maps a scan-line distance to its shortest distance code, for one width.
pub(crate) struct DistanceCoder {
    /// `plane[d]`: the smallest code 1..=120 whose distance is `d`, or 0.
    plane: Vec<u8>,
}

impl DistanceCoder {
    pub(crate) fn new(width: usize) -> Self {
        let mut plane = vec![0u8; 8 * width + 9];
        for code in (1..=PLANE_CODES).rev() {
            let d = code_to_distance(code, width);
            if d < plane.len() {
                plane[d] = code as u8;
            }
        }
        DistanceCoder { plane }
    }

    #[inline]
    pub(crate) fn code(&self, dist: usize) -> u32 {
        match self.plane.get(dist) {
            Some(&c) if c != 0 => u32::from(c),
            _ => (dist + PLANE_CODES) as u32,
        }
    }

    /// Whether a distance has a neighbourhood code (and is cheap).
    #[inline]
    fn is_plane(&self, dist: usize) -> bool {
        self.plane.get(dist).is_some_and(|&c| c != 0)
    }
}

#[inline]
fn hash(a: u32, b: u32) -> usize {
    (a.wrapping_mul(0x9E37_79B1) ^ b.wrapping_mul(0x85EB_CA77).rotate_left(7))
        .wrapping_mul(0x2C1B_3C6D) as usize
        >> (32 - HASH_BITS)
}

#[inline]
fn match_len(px: &[u32], a: usize, b: usize, max: usize) -> usize {
    let mut n = 0;
    while n < max && px[a + n] == px[b + n] {
        n += 1;
    }
    n
}

/// Bit costs from a first pass's statistics, for a second pass that
/// weighs a copy against the literals it replaces.
pub(crate) struct CostModel {
    /// Cost of each pixel as a literal, as prefix sums: `lit[j] - lit[i]`
    /// is pixels i..j.
    lit: Vec<f32>,
    /// Cost of each length prefix code, with its extra bits.
    len: [f32; 24],
    /// Cost of each distance prefix code, with its extra bits.
    dist: [f32; 40],
}

impl CostModel {
    /// `costs` per alphabet (green, red, blue, alpha, distance), in bits.
    pub(crate) fn new(px: &[u32], costs: &[Vec<f32>; 5]) -> Self {
        let mut lit = Vec::with_capacity(px.len() + 1);
        let mut acc = 0f32;
        lit.push(0.0);
        for &p in px {
            acc += costs[0][((p >> 8) & 0xff) as usize]
                + costs[1][((p >> 16) & 0xff) as usize]
                + costs[2][(p & 0xff) as usize]
                + costs[3][(p >> 24) as usize];
            lit.push(acc);
        }
        let mut len = [0f32; 24];
        for (c, l) in len.iter_mut().enumerate() {
            *l = costs[0][256 + c] + super::prefix_base(c).1 as f32;
        }
        let mut dist = [0f32; 40];
        for (c, d) in dist.iter_mut().enumerate() {
            *d = costs[4][c] + super::prefix_base(c).1 as f32;
        }
        CostModel { lit, len, dist }
    }

    #[inline]
    fn copy(&self, len: usize, dist_code: u32) -> f32 {
        self.len[super::prefix_encode(len).0]
            + self.dist[super::prefix_encode(dist_code as usize).0]
    }

    /// The literal cost of pixels `i..j`.
    #[inline]
    fn literals(&self, i: usize, j: usize) -> f32 {
        self.lit[j] - self.lit[i]
    }
}

struct Finder<'a> {
    px: &'a [u32],
    width: usize,
    /// Matches end here (the end of the segment being coded).
    end: usize,
    /// The first position in the chains; `prev` is indexed from it.
    base: usize,
    head: Vec<u32>,
    prev: Vec<u32>,
    chain: usize,
    nice: usize,
    coder: &'a DistanceCoder,
    model: Option<&'a CostModel>,
}

const NONE: u32 = u32::MAX;

/// A candidate copy: length, distance, and how much better than literals
/// it is (bits saved with a cost model, else a heuristic score).
#[derive(Clone, Copy)]
struct Match {
    len: usize,
    dist: usize,
    score: f32,
}

const NO_MATCH: Match = Match {
    len: 0,
    dist: 0,
    score: 0.0,
};

impl Finder<'_> {
    fn insert(&mut self, i: usize) {
        if self.chain == 0 || i + 1 >= self.px.len() {
            return;
        }
        let h = hash(self.px[i], self.px[i + 1]);
        self.prev[i - self.base] = self.head[h];
        self.head[h] = i as u32;
    }

    #[inline]
    fn score(&self, i: usize, len: usize, dist: usize) -> f32 {
        match self.model {
            Some(m) => m.literals(i, i + len) - m.copy(len, self.coder.code(dist)),
            None => {
                let plane = self.coder.is_plane(dist);
                if len < if plane { 2 } else { 3 } {
                    return 0.0;
                }
                // About a byte of description per far distance code bit.
                let penalty = if plane {
                    0.0
                } else {
                    1.0 + ((usize::BITS - dist.leading_zeros()) / 4) as f32
                };
                len as f32 * 4.0 - penalty
            }
        }
    }

    /// The best copy at `i`, or a length of 0.
    fn best(&self, i: usize) -> Match {
        let max = (self.end - i).min(MAX_LENGTH);
        if max < 2 {
            return NO_MATCH;
        }
        let mut best = NO_MATCH;
        let consider = |len: usize, dist: usize, best: &mut Match| {
            if len == 0 {
                return;
            }
            let score = self.score(i, len, dist);
            if score > best.score {
                *best = Match { len, dist, score };
            }
        };
        if i >= 1 {
            consider(match_len(self.px, i, i - 1, max), 1, &mut best);
        }
        if i >= self.width && self.width > 1 {
            consider(
                match_len(self.px, i, i - self.width, max),
                self.width,
                &mut best,
            );
        }
        if self.chain > 0 && best.len < max {
            let mut cand = self.head[hash(self.px[i], self.px[i + 1])];
            let mut steps = 0;
            while cand != NONE && steps < self.chain {
                let c = cand as usize;
                let dist = i - c;
                if dist > MAX_DISTANCE || c < self.base {
                    break;
                }
                // Cheap reject: the pixel that would extend the best.
                let probe = best.len.min(max - 1);
                if self.px[c + probe] == self.px[i + probe] {
                    consider(match_len(self.px, i, c, max), dist, &mut best);
                    if best.len >= self.nice.min(max) {
                        break;
                    }
                }
                cand = self.prev[c - self.base];
                steps += 1;
            }
        }
        best
    }
}

/// Pixels per segment: images larger than this are matched in segments of
/// at least this many pixels (and at most 32 segments), each with every
/// earlier pixel in reach (up to the largest distance) as history, on
/// several threads. Matches end at segment boundaries; the result does not
/// depend on the number of threads.
const SEGMENT: usize = 1 << 17;

/// Literals and copies for `px`, an image `width` wide. With a cost model,
/// a copy is taken when it saves bits over the literals it replaces;
/// without, by a length heuristic.
pub(crate) fn backward_references(
    px: &[u32],
    width: usize,
    params: &MatchParams,
    coder: &DistanceCoder,
    model: Option<&CostModel>,
) -> Vec<Token> {
    let n = px.len();
    let seg = SEGMENT.max(n.div_ceil(32));
    let segments = n.div_ceil(seg).max(1);
    let parts = crate::par::map(segments, 0, |k| {
        let (start, end) = (k * seg, ((k + 1) * seg).min(n));
        segment_references(px, width, params, coder, model, start, end)
    });
    let mut out = Vec::with_capacity(parts.iter().map(Vec::len).sum());
    for p in parts {
        out.extend_from_slice(&p);
    }
    out
}

/// [`backward_references`] for pixels `start..end`.
fn segment_references(
    px: &[u32],
    width: usize,
    params: &MatchParams,
    coder: &DistanceCoder,
    model: Option<&CostModel>,
    start: usize,
    end: usize,
) -> Vec<Token> {
    let base = start.saturating_sub(MAX_DISTANCE);
    let mut f = Finder {
        px,
        width,
        end,
        base,
        head: if params.chain > 0 {
            vec![NONE; 1 << HASH_BITS]
        } else {
            Vec::new()
        },
        prev: if params.chain > 0 {
            vec![NONE; end - base]
        } else {
            Vec::new()
        },
        chain: params.chain,
        nice: params.nice,
        coder,
        model,
    };
    for k in base..start {
        f.insert(k);
    }
    let n = end;
    let mut out = Vec::with_capacity((end - start) / 2 + 16);
    let mut i = start;
    let mut pending: Option<Match> = None;
    while i < n {
        let m = pending.take().unwrap_or_else(|| f.best(i));
        if m.len == 0 {
            out.push(Token::Literal(px[i]));
            f.insert(i);
            i += 1;
            continue;
        }
        if m.len < params.lazy_below && i + 1 < n {
            f.insert(i);
            let next = f.best(i + 1);
            let better = match model {
                // A literal at i, then the next copy, against this copy.
                Some(md) => {
                    next.len > 0
                        && next.score - md.literals(i, i + 1)
                            > m.score - md.literals(i + m.len.min(next.len + 1), i + m.len)
                }
                None => next.len > m.len + 1,
            };
            if better {
                out.push(Token::Literal(px[i]));
                i += 1;
                pending = Some(next);
                continue;
            }
            out.push(Token::Copy {
                len: m.len as u32,
                dist_code: coder.code(m.dist),
            });
            for k in i + 1..i + m.len {
                f.insert(k);
            }
        } else {
            out.push(Token::Copy {
                len: m.len as u32,
                dist_code: coder.code(m.dist),
            });
            for k in i..i + m.len {
                f.insert(k);
            }
        }
        i += m.len;
    }
    out
}

/// Replaces literals that the colour cache holds with cache indices.
pub(crate) fn apply_cache(tokens: &[Token], px: &[u32], bits: u32) -> Vec<Token> {
    if bits == 0 {
        return tokens.to_vec();
    }
    let mut cache = vec![0u32; 1 << bits];
    let mut out = Vec::with_capacity(tokens.len());
    let mut pos = 0;
    for t in tokens {
        match *t {
            Token::Literal(_) | Token::Cache(_) => {
                let p = px[pos];
                let k = cache_index(p, bits);
                if cache[k] == p {
                    out.push(Token::Cache(k as u32));
                } else {
                    out.push(Token::Literal(p));
                    cache[k] = p;
                }
                pos += 1;
            }
            Token::Copy { len, .. } => {
                for &p in &px[pos..pos + len as usize] {
                    cache[cache_index(p, bits)] = p;
                }
                out.push(*t);
                pos += len as usize;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expand(tokens: &[Token], width: usize) -> Vec<u32> {
        let mut out: Vec<u32> = Vec::new();
        for t in tokens {
            match *t {
                Token::Literal(p) => out.push(p),
                Token::Cache(_) => unreachable!(),
                Token::Copy { len, dist_code } => {
                    let d = code_to_distance(dist_code as usize, width);
                    for _ in 0..len {
                        out.push(out[out.len() - d]);
                    }
                }
            }
        }
        out
    }

    #[test]
    fn references_reproduce_the_pixels() {
        let w = 23;
        let mut px: Vec<u32> = Vec::new();
        let mut s = 1u32;
        for i in 0..w * 40 {
            s = s.wrapping_mul(1_103_515_245).wrapping_add(12345);
            px.push(if i % 7 < 3 {
                5
            } else if i > w * 10 && i % w < 9 {
                px[i - w]
            } else {
                s >> 28
            });
        }
        for effort in 0..=6 {
            let coder = DistanceCoder::new(w);
            let t = backward_references(&px, w, &MatchParams::for_effort(effort), &coder, None);
            assert_eq!(expand(&t, w), px, "effort {effort}");
            assert!(t.len() < px.len());
        }
    }

    #[test]
    fn segmented_references_reproduce_the_pixels() {
        // Over three segments: rows repeating with a shift, runs and noise,
        // so copies reach back across segment boundaries.
        let w = 613;
        let h = (3 * SEGMENT) / w + 7;
        let mut s = 7u32;
        let mut px: Vec<u32> = Vec::with_capacity(w * h);
        for i in 0..w * h {
            s = s.wrapping_mul(1_103_515_245).wrapping_add(12345);
            px.push(if i > w && i % 5 != 0 {
                px[i - w + 1]
            } else if i % 11 < 4 {
                9
            } else {
                s >> 26
            });
        }
        for effort in [0, 4] {
            let coder = DistanceCoder::new(w);
            let t = backward_references(&px, w, &MatchParams::for_effort(effort), &coder, None);
            assert_eq!(expand(&t, w), px, "effort {effort}");
            assert!(t.len() < px.len(), "effort {effort}: {} tokens", t.len());
        }
    }
}
