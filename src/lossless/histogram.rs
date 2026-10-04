//! Symbol statistics for the encoder: the five histograms of a prefix code
//! group and the encoder's estimate of what coding them costs.

use super::lz77::Token;
use super::{NUM_DISTANCE_CODES, NUM_LENGTH_CODES, NUM_LITERALS, prefix_encode};

/// Counts for one prefix code group: green (with length and cache
/// symbols), red, blue, alpha, distance.
#[derive(Clone, Debug)]
pub(crate) struct Histogram {
    pub(crate) codes: [Vec<u32>; 5],
    /// Raw extra bits of lengths and distances.
    pub(crate) extra_bits: u64,
}

impl Histogram {
    pub(crate) fn new(cache_bits: u32) -> Self {
        let cache = if cache_bits > 0 {
            1usize << cache_bits
        } else {
            0
        };
        Histogram {
            codes: [
                vec![0; NUM_LITERALS + NUM_LENGTH_CODES + cache],
                vec![0; NUM_LITERALS],
                vec![0; NUM_LITERALS],
                vec![0; NUM_LITERALS],
                vec![0; NUM_DISTANCE_CODES],
            ],
            extra_bits: 0,
        }
    }

    #[inline]
    pub(crate) fn add(&mut self, t: &Token) {
        match *t {
            Token::Literal(p) => {
                self.codes[0][((p >> 8) & 0xff) as usize] += 1;
                self.codes[1][((p >> 16) & 0xff) as usize] += 1;
                self.codes[2][(p & 0xff) as usize] += 1;
                self.codes[3][(p >> 24) as usize] += 1;
            }
            Token::Cache(i) => self.codes[0][NUM_LITERALS + NUM_LENGTH_CODES + i as usize] += 1,
            Token::Copy { len, dist_code } => {
                let (lc, lb, _) = prefix_encode(len as usize);
                let (dc, db, _) = prefix_encode(dist_code as usize);
                self.codes[0][NUM_LITERALS + lc] += 1;
                self.codes[4][dc] += 1;
                self.extra_bits += u64::from(lb + db);
            }
        }
    }

    pub(crate) fn merge(&mut self, other: &Histogram) {
        for (a, b) in self.codes.iter_mut().zip(&other.codes) {
            for (x, y) in a.iter_mut().zip(b) {
                *x += y;
            }
        }
        self.extra_bits += other.extra_bits;
    }

    /// Estimated bits to code everything counted, prefix code
    /// descriptions included.
    pub(crate) fn cost(&self) -> f64 {
        self.codes.iter().map(|c| population_cost(c)).sum::<f64>() + self.extra_bits as f64
    }

    /// The cost of the union of `self` and `other`, without building it.
    pub(crate) fn merged_cost(&self, other: &Histogram) -> f64 {
        let mut total = (self.extra_bits + other.extra_bits) as f64;
        for (a, b) in self.codes.iter().zip(&other.codes) {
            total += population_cost_pair(a, b);
        }
        total
    }
}

/// `n * log2(n)`, from a table for small `n` (the table holds this same
/// computation's results, so the values are identical).
#[inline]
pub(crate) fn nlog2n(n: u32) -> f64 {
    const SMALL: usize = 4096;
    static TABLE: std::sync::OnceLock<Vec<f64>> = std::sync::OnceLock::new();
    if (n as usize) < SMALL {
        return TABLE.get_or_init(|| (0..SMALL as u32).map(nlog2n_direct).collect())[n as usize];
    }
    nlog2n_direct(n)
}

fn nlog2n_direct(n: u32) -> f64 {
    if n <= 1 {
        0.0
    } else {
        let n = f64::from(n);
        n * n.log2()
    }
}

/// Bits a prefix code spends on `counts` (the Shannon bound) plus an
/// estimate of the code's description: next to nothing for zero, one or
/// two symbols (the simple code length code), otherwise a few bits per
/// used symbol and a fixed part.
pub(crate) fn population_cost(counts: &[u32]) -> f64 {
    let (mut total, mut sum, mut used) = (0u64, 0f64, 0u32);
    for &c in counts {
        if c > 0 {
            total += u64::from(c);
            sum += nlog2n(c);
            used += 1;
        }
    }
    finish_cost(total, sum, used)
}

fn population_cost_pair(a: &[u32], b: &[u32]) -> f64 {
    let (mut total, mut sum, mut used) = (0u64, 0f64, 0u32);
    for (&x, &y) in a.iter().zip(b) {
        let c = x + y;
        if c > 0 {
            total += u64::from(c);
            sum += nlog2n(c);
            used += 1;
        }
    }
    finish_cost(total, sum, used)
}

#[inline]
fn finish_cost(total: u64, sum: f64, used: u32) -> f64 {
    let header = match used {
        0 | 1 => 4.0,
        2 => 20.0,
        n => 30.0 + 3.5 * f64::from(n),
    };
    let data = if total == 0 || used <= 1 {
        0.0
    } else {
        nlog2n(total.min(u64::from(u32::MAX)) as u32) - sum
    };
    data + header
}

/// Shannon bits of one 256-bin histogram (no description cost), with
/// `nlog` a table of `n log2 n` covering the total.
#[inline]
pub(crate) fn shannon_bits(counts: &[u32; 256], total: u32, nlog: &[f64]) -> f64 {
    let mut s = 0.0;
    for &c in counts {
        if c > 0 {
            s += nlog[c as usize];
        }
    }
    nlog[total as usize] - s
}

/// A table of `n log2 n` for n in 0..=max.
pub(crate) fn nlog2n_table(max: usize) -> Vec<f64> {
    (0..=max).map(|n| nlog2n(n as u32)).collect()
}
