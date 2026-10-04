//! The lossless bitstream's bit order (RFC 9649 section 3.2): bytes in
//! stream order, the bits of each byte least significant first, and a field
//! of n bits assembled with its first bit read as its least significant.

/// Reads a VP8L bitstream. Past the end of the data it reads zeros and
/// remembers that it did: [`BitReader::overrun`] tells, and the decoder
/// checks it where the stream must be whole (a corrupt stream ends in an
/// error, not in a panic or an endless loop, because every loop that reads
/// is bounded by the picture's size).
pub(crate) struct BitReader<'a> {
    data: &'a [u8],
    /// The next byte to load into `buf`.
    pos: usize,
    /// Bits not yet consumed, the next one in bit 0.
    buf: u64,
    /// How many bits of `buf` are valid.
    nbits: u32,
}

impl<'a> BitReader<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        let mut r = BitReader {
            data,
            pos: 0,
            buf: 0,
            nbits: 0,
        };
        r.refill();
        r
    }

    /// Tops `buf` up to at least 56 valid bits.
    #[inline(always)]
    pub(crate) fn refill(&mut self) {
        if self.nbits > 56 {
            return;
        }
        if self.pos + 8 <= self.data.len() {
            let word = u64::from_le_bytes(self.data[self.pos..self.pos + 8].try_into().unwrap());
            self.buf |= word << self.nbits;
            let take = (63 - self.nbits) >> 3;
            self.pos += take as usize;
            self.nbits += take * 8;
        } else {
            while self.nbits <= 56 {
                let b = self.data.get(self.pos).copied().unwrap_or(0);
                self.buf |= u64::from(b) << self.nbits;
                self.pos += 1;
                self.nbits += 8;
            }
        }
    }

    /// The next `n` bits (at most 32) without consuming them. The caller
    /// has refilled.
    #[inline(always)]
    pub(crate) fn peek(&self, n: u32) -> u32 {
        (self.buf & ((1u64 << n) - 1)) as u32
    }

    /// Drops `n` bits (no more than are buffered).
    #[inline(always)]
    pub(crate) fn consume(&mut self, n: u32) {
        self.buf >>= n;
        self.nbits -= n;
    }

    /// Reads an `n`-bit field, n at most 32.
    #[inline(always)]
    pub(crate) fn read(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        self.refill();
        let v = self.peek(n);
        self.consume(n);
        v
    }

    /// Whether more bits have been consumed than the data holds.
    pub(crate) fn overrun(&self) -> bool {
        (self.pos as u64) * 8 - u64::from(self.nbits) > (self.data.len() as u64) * 8
    }
}

/// Writes a VP8L bitstream.
pub(crate) struct BitWriter {
    out: Vec<u8>,
    buf: u64,
    nbits: u32,
}

impl BitWriter {
    pub(crate) fn new() -> Self {
        BitWriter {
            out: Vec::new(),
            buf: 0,
            nbits: 0,
        }
    }

    /// Appends the low `n` bits of `v` (n at most 32).
    #[inline(always)]
    pub(crate) fn write(&mut self, v: u32, n: u32) {
        debug_assert!(n <= 32);
        debug_assert!(n == 32 || v >> n == 0, "value {v} wider than {n} bits");
        self.buf |= u64::from(v) << self.nbits;
        self.nbits += n;
        if self.nbits >= 32 {
            self.out.extend_from_slice(&(self.buf as u32).to_le_bytes());
            self.buf >>= 32;
            self.nbits -= 32;
        }
    }

    /// The bytes, the last one zero-padded.
    pub(crate) fn finish(mut self) -> Vec<u8> {
        while self.nbits > 0 {
            self.out.push(self.buf as u8);
            self.buf >>= 8;
            self.nbits = self.nbits.saturating_sub(8);
        }
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_round_trip_lsb_first() {
        let mut w = BitWriter::new();
        let fields: Vec<(u32, u32)> = (0..500u32)
            .map(|i| {
                (
                    i.wrapping_mul(2654435761) >> (32 - (i % 32 + 1)),
                    i % 32 + 1,
                )
            })
            .collect();
        for &(v, n) in &fields {
            w.write(v, n);
        }
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        for &(v, n) in &fields {
            assert_eq!(r.read(n), v);
        }
        assert!(!r.overrun());
        r.read(32);
        r.read(32);
        assert!(r.overrun());
    }

    #[test]
    fn rfc_example_bit_order() {
        // ReadBits(2) == ReadBits(1) | ReadBits(1) << 1.
        let mut r = BitReader::new(&[0b0000_0110]);
        assert_eq!(r.read(1), 0);
        assert_eq!(r.read(1), 1);
        assert_eq!(r.read(1), 1);
        let mut r = BitReader::new(&[0b0000_0110]);
        assert_eq!(r.read(3), 0b110);
    }
}
