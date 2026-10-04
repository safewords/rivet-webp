//! Canonical prefix codes (RFC 9649 section 3.7): building a decoding table
//! from code lengths, and, for the encoder, length-limited code lengths from
//! symbol counts and the codes they imply.
//!
//! The RFC says the codes are canonical [Huffman] codes sent as lengths and
//! does not spell the assignment out; it is the usual one (shorter codes
//! first, within a length in symbol order, each code the previous plus one),
//! with a code's first bit in the stream being its most significant — the
//! convention of DEFLATE. Google's test files decode under it
//! (docs/PROVENANCE.md).

use crate::bits::BitReader;
use crate::error::{Result, bitstream};

/// Longest code a VP8L prefix code may have: lengths are coded 0..=15.
pub(crate) const MAX_LENGTH: u32 = 15;

/// Bits the first-level table resolves.
const ROOT_BITS: u32 = 8;

/// A table entry: the symbol and its length, or for a code longer than the
/// root a link to a second-level table.
#[derive(Clone, Copy, Default)]
struct Entry {
    /// Code length, or for a link `ROOT_BITS +` the subtable's index bits.
    len: u8,
    /// The symbol, or for a link the subtable's offset.
    value: u16,
    link: bool,
}

/// A decoding table for one prefix code.
#[derive(Clone)]
pub(crate) struct HuffmanTable {
    table: Vec<Entry>,
    /// The only symbol of a one-symbol code, which reads no bits.
    single: Option<u16>,
}

impl HuffmanTable {
    /// Builds the table for `lengths` (one per symbol, 0 = unused). The code
    /// must be complete — every bit string decodes — unless it has a single
    /// symbol, which RFC 9649 section 3.7.2.1 calls a complete tree of one
    /// leaf that consumes no bits.
    pub(crate) fn new(lengths: &[u8]) -> Result<HuffmanTable> {
        let mut count = [0u32; MAX_LENGTH as usize + 1];
        let mut used = 0usize;
        let mut last = 0usize;
        for (s, &l) in lengths.iter().enumerate() {
            if l as u32 > MAX_LENGTH {
                return Err(bitstream("prefix code length above 15"));
            }
            if l > 0 {
                count[l as usize] += 1;
                used += 1;
                last = s;
            }
        }
        if used == 0 {
            return Err(bitstream("prefix code with no symbols"));
        }
        if used == 1 {
            return Ok(HuffmanTable {
                table: Vec::new(),
                single: Some(last as u16),
            });
        }
        // Kraft: the code is complete when the lengths fill the tree exactly.
        let mut room: i64 = 1;
        for &c in count.iter().skip(1) {
            room = room * 2 - i64::from(c);
            if room < 0 {
                return Err(bitstream("prefix code over-subscribed"));
            }
        }
        if room != 0 {
            return Err(bitstream("prefix code incomplete"));
        }
        let mut next = [0u32; MAX_LENGTH as usize + 2];
        let mut code = 0u32;
        for len in 1..=MAX_LENGTH as usize {
            code = (code + count[len - 1]) << 1;
            next[len] = code;
        }
        // Codes as read: bit-reversed, the first bit in the stream lowest.
        let mut codes: Vec<(u32, u32, u16)> = Vec::with_capacity(used);
        for (s, &l) in lengths.iter().enumerate() {
            if l > 0 {
                let l = u32::from(l);
                let c = next[l as usize];
                next[l as usize] += 1;
                codes.push((reverse(c, l), l, s as u16));
            }
        }
        let root_size = 1usize << ROOT_BITS;
        let mut table = vec![Entry::default(); root_size];
        // The longest code under each root prefix sizes its subtable.
        let mut sub_bits = vec![0u32; root_size];
        for &(r, l, _) in &codes {
            if l > ROOT_BITS {
                let i = (r as usize) & (root_size - 1);
                sub_bits[i] = sub_bits[i].max(l - ROOT_BITS);
            }
        }
        for (i, &b) in sub_bits.iter().enumerate() {
            if b > 0 {
                let offset = table.len();
                table[i] = Entry {
                    len: (ROOT_BITS + b) as u8,
                    value: offset as u16,
                    link: true,
                };
                table.resize(offset + (1 << b), Entry::default());
            }
        }
        for &(r, l, s) in &codes {
            if l <= ROOT_BITS {
                let mut k = r as usize;
                while k < root_size {
                    table[k] = Entry {
                        len: l as u8,
                        value: s,
                        link: false,
                    };
                    k += 1 << l;
                }
            } else {
                let root = table[(r as usize) & (root_size - 1)];
                let b = u32::from(root.len) - ROOT_BITS;
                let base = root.value as usize;
                let mut k = (r >> ROOT_BITS) as usize;
                let step = 1usize << (l - ROOT_BITS);
                while k < 1 << b {
                    table[base + k] = Entry {
                        len: (l - ROOT_BITS) as u8,
                        value: s,
                        link: false,
                    };
                    k += step;
                }
            }
        }
        Ok(HuffmanTable {
            table,
            single: None,
        })
    }

    /// Reads one symbol.
    #[inline(always)]
    pub(crate) fn read(&self, br: &mut BitReader<'_>) -> u16 {
        if let Some(s) = self.single {
            return s;
        }
        br.refill();
        let bits = br.peek(MAX_LENGTH);
        let e = self.table[(bits & ((1 << ROOT_BITS) - 1)) as usize];
        if !e.link {
            br.consume(u32::from(e.len));
            return e.value;
        }
        let sub = u32::from(e.len) - ROOT_BITS;
        let e2 = self.table[e.value as usize + ((bits >> ROOT_BITS) & ((1 << sub) - 1)) as usize];
        br.consume(ROOT_BITS + u32::from(e2.len));
        e2.value
    }

    /// The symbol of a one-symbol code.
    pub(crate) fn single(&self) -> Option<u16> {
        self.single
    }
}

/// The low `len` bits of `code`, reversed.
pub(crate) fn reverse(code: u32, len: u32) -> u32 {
    if len == 0 {
        return 0;
    }
    code.reverse_bits() >> (32 - len)
}

/// Canonical codes for `lengths`, each bit-reversed for an LSB-first
/// writer (zero for an unused symbol).
pub(crate) fn codes_from_lengths(lengths: &[u8]) -> Vec<u16> {
    let mut count = [0u32; MAX_LENGTH as usize + 1];
    for &l in lengths {
        if l > 0 {
            count[l as usize] += 1;
        }
    }
    let mut next = [0u32; MAX_LENGTH as usize + 2];
    let mut code = 0u32;
    for len in 1..=MAX_LENGTH as usize {
        code = (code + count[len - 1]) << 1;
        next[len] = code;
    }
    lengths
        .iter()
        .map(|&l| {
            if l == 0 {
                0
            } else {
                let c = next[l as usize];
                next[l as usize] += 1;
                reverse(c, u32::from(l)) as u16
            }
        })
        .collect()
}

/// Code lengths for symbol `counts`, none longer than `max_len`: a Huffman
/// code, and where that is too deep, the counts are flattened (each raised
/// toward a floor that doubles until the code fits), which keeps the order
/// of the lengths and loses little. Symbols with a count of zero get no
/// code. A single used symbol gets length 1 (RFC 9649's one-leaf tree).
pub(crate) fn lengths_from_counts(counts: &[u32], max_len: u32) -> Vec<u8> {
    let used: Vec<usize> = (0..counts.len()).filter(|&i| counts[i] > 0).collect();
    let mut lengths = vec![0u8; counts.len()];
    match used.len() {
        0 => return lengths,
        1 => {
            lengths[used[0]] = 1;
            return lengths;
        }
        _ => {}
    }
    let mut floor = 0u32;
    loop {
        let weights: Vec<u64> = used
            .iter()
            .map(|&i| u64::from(counts[i].max(floor)))
            .collect();
        let depth = huffman_depths(&weights);
        if depth.iter().all(|&d| d <= max_len) {
            for (k, &i) in used.iter().enumerate() {
                lengths[i] = depth[k] as u8;
            }
            return lengths;
        }
        floor = if floor == 0 { 1 } else { floor * 2 };
    }
}

/// Depths of a Huffman tree over `weights` (at least two), by the
/// two-queue method on sorted leaves.
fn huffman_depths(weights: &[u64]) -> Vec<u32> {
    let n = weights.len();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| (weights[i], i));
    // Nodes: leaves 0..n (in sorted order), internal n..2n-1.
    let mut weight = Vec::with_capacity(2 * n);
    let mut parent = vec![0usize; 2 * n - 1];
    for &i in &order {
        weight.push(weights[i]);
    }
    let (mut leaf, mut inner) = (0usize, n);
    let take = |weight: &Vec<u64>, leaf: &mut usize, inner: &mut usize| -> usize {
        if *leaf < n && (*inner >= weight.len() || weight[*leaf] <= weight[*inner]) {
            *leaf += 1;
            *leaf - 1
        } else {
            *inner += 1;
            *inner - 1
        }
    };
    for _ in 0..n - 1 {
        let a = take(&weight, &mut leaf, &mut inner);
        let b = take(&weight, &mut leaf, &mut inner);
        let id = weight.len();
        weight.push(weight[a] + weight[b]);
        parent[a] = id;
        parent[b] = id;
    }
    let root = 2 * n - 2;
    let mut depth = vec![0u32; 2 * n - 1];
    for id in (0..root).rev() {
        depth[id] = depth[parent[id]] + 1;
    }
    let mut out = vec![0u32; n];
    for (k, &i) in order.iter().enumerate() {
        out[i] = depth[k];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bits::BitWriter;

    fn round_trip(counts: &[u32], max_len: u32) {
        let lengths = lengths_from_counts(counts, max_len);
        assert!(lengths.iter().all(|&l| u32::from(l) <= max_len));
        let codes = codes_from_lengths(&lengths);
        let table = HuffmanTable::new(&lengths).unwrap();
        let symbols: Vec<usize> = (0..counts.len())
            .filter(|&s| counts[s] > 0)
            .cycle()
            .take(2000)
            .collect();
        let mut w = BitWriter::new();
        let single = lengths.iter().filter(|&&l| l > 0).count() == 1;
        for &s in &symbols {
            if !single {
                w.write(u32::from(codes[s]), u32::from(lengths[s]));
            }
        }
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        for &s in &symbols {
            assert_eq!(table.read(&mut r) as usize, s);
        }
    }

    #[test]
    fn codes_round_trip() {
        round_trip(&[5, 0, 0, 1], 15);
        round_trip(&[0, 0, 7], 15);
        let geometric: Vec<u32> = (0..40).map(|i| 1u32 << (i % 31)).collect();
        round_trip(&geometric, 15);
        round_trip(&geometric, 7);
        let flat: Vec<u32> = (0..280).map(|i| 1 + i % 3).collect();
        round_trip(&flat, 15);
        let fib: Vec<u32> = {
            let mut v = vec![1u32, 1];
            while v.len() < 30 {
                let n = v[v.len() - 1].saturating_add(v[v.len() - 2]);
                v.push(n);
            }
            v
        };
        round_trip(&fib, 15);
    }

    #[test]
    fn incomplete_and_oversubscribed_codes_are_refused() {
        assert!(HuffmanTable::new(&[1, 2, 0]).is_err());
        assert!(HuffmanTable::new(&[1, 1, 1]).is_err());
        assert!(HuffmanTable::new(&[0, 0]).is_err());
        assert!(HuffmanTable::new(&[0, 1]).is_ok());
    }
}
