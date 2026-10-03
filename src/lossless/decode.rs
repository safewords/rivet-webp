//! The VP8L decoder (RFC 9649 section 3).

use super::transform::{add_green, inverse_color, inverse_color_indexing, inverse_predictor, bundle_bits};
use super::*;
use crate::bits::BitReader;
use crate::error::{Result, bitstream, limit, unsupported};
use crate::huffman::HuffmanTable;

/// What the 5-byte VP8L header says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Header {
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// The `alpha_is_used` hint.
    pub(crate) alpha_hint: bool,
}

/// Reads the signature, size, alpha hint and version (section 3.4).
pub(crate) fn read_header(data: &[u8]) -> Result<Header> {
    if data.len() < 5 {
        return Err(bitstream("VP8L bitstream shorter than its 5-byte header"));
    }
    if data[0] != SIGNATURE {
        return Err(bitstream(format!("VP8L signature 0x{:02x}, not 0x2f", data[0])));
    }
    let bits = u32::from_le_bytes(data[1..5].try_into().unwrap());
    let width = (bits & 0x3fff) + 1;
    let height = ((bits >> 14) & 0x3fff) + 1;
    let alpha_hint = (bits >> 28) & 1 == 1;
    let version = bits >> 29;
    if version != 0 {
        return Err(unsupported(format!("VP8L version {version} (only 0 is defined)")));
    }
    Ok(Header {
        width,
        height,
        alpha_hint,
    })
}

/// Decodes a whole VP8L bitstream (header included) to ARGB pixels.
pub(crate) fn decode(data: &[u8], max_pixels: u64) -> Result<(Header, Vec<u32>)> {
    let header = read_header(data)?;
    let pixels = u64::from(header.width) * u64::from(header.height);
    if pixels > max_pixels {
        return Err(limit(format!(
            "lossless image {}x{} has {pixels} pixels, the limit is {max_pixels}",
            header.width, header.height
        )));
    }
    let mut br = BitReader::new(&data[5..]);
    let argb = decode_image_stream(&mut br, header.width as usize, header.height as usize)?;
    Ok((header, argb))
}

/// Decodes a headerless image-stream of known size: the form of an `ALPH`
/// chunk's lossless payload (section 2.7.1.2).
pub(crate) fn decode_headerless(data: &[u8], width: usize, height: usize) -> Result<Vec<u32>> {
    let mut br = BitReader::new(data);
    decode_image_stream(&mut br, width, height)
}

enum Transform {
    Predictor { bits: u32, width: usize, image: Vec<u32> },
    Color { bits: u32, width: usize, image: Vec<u32> },
    SubtractGreen,
    ColorIndexing { bits: u32, width: usize, table: Vec<u32> },
}

/// `image-stream`: the transforms, then the spatially coded image, then the
/// inverse transforms, last read first applied.
fn decode_image_stream(br: &mut BitReader<'_>, width: usize, height: usize) -> Result<Vec<u32>> {
    let mut xsize = width;
    let mut transforms = Vec::new();
    let mut seen = 0u32;
    while br.read(1) == 1 {
        let kind = br.read(2);
        if seen & (1 << kind) != 0 {
            return Err(bitstream("a VP8L transform used twice"));
        }
        seen |= 1 << kind;
        match kind {
            0 | 1 => {
                let bits = br.read(3) + 2;
                let image = decode_coded_image(br, subsample(xsize, bits), subsample(height, bits), false)?;
                transforms.push(if kind == 0 {
                    Transform::Predictor { bits, width: xsize, image }
                } else {
                    Transform::Color { bits, width: xsize, image }
                });
            }
            2 => transforms.push(Transform::SubtractGreen),
            _ => {
                let size = br.read(8) as usize + 1;
                let mut table = decode_coded_image(br, size, 1, false)?;
                for i in 1..table.len() {
                    table[i] = transform::add_pixels(table[i], table[i - 1]);
                }
                let bits = bundle_bits(size);
                transforms.push(Transform::ColorIndexing { bits, width: xsize, table });
                xsize = subsample(xsize, bits);
            }
        }
    }
    let mut px = decode_coded_image(br, xsize, height, true)?;
    for t in transforms.iter().rev() {
        match t {
            Transform::Predictor { bits, width, image } => inverse_predictor(&mut px, *width, height, *bits, image),
            Transform::Color { bits, width, image } => inverse_color(&mut px, *width, height, *bits, image),
            Transform::SubtractGreen => add_green(&mut px),
            Transform::ColorIndexing { bits, width, table } => {
                px = inverse_color_indexing(&px, *width, height, *bits, table);
            }
        }
    }
    Ok(px)
}

/// The five prefix codes of a group (section 3.7.2).
struct Group {
    green: HuffmanTable,
    red: HuffmanTable,
    blue: HuffmanTable,
    alpha: HuffmanTable,
    distance: HuffmanTable,
    /// When red, blue and alpha each have one symbol, the bits they
    /// contribute to every literal.
    fixed_rba: Option<u32>,
}

/// `spatially-coded-image` (level 0, `meta` true) or `entropy-coded-image`.
fn decode_coded_image(br: &mut BitReader<'_>, width: usize, height: usize, meta: bool) -> Result<Vec<u32>> {
    let cache_bits = if br.read(1) == 1 {
        let b = br.read(4);
        if !(1..=11).contains(&b) {
            return Err(bitstream(format!("colour cache of {b} bits (1 to 11 allowed)")));
        }
        b
    } else {
        0
    };
    let cache_size = if cache_bits > 0 { 1usize << cache_bits } else { 0 };

    // The entropy image, and which of its groups are used.
    let mut entropy: Option<(u32, usize, Vec<u32>)> = None;
    let mut num_groups = 1usize;
    if meta && br.read(1) == 1 {
        let bits = br.read(3) + 2;
        let pw = subsample(width, bits);
        let image = decode_coded_image(br, pw, subsample(height, bits), false)?;
        num_groups = image.iter().map(|&p| ((p >> 8) & 0xffff) as usize).max().unwrap_or(0) + 1;
        entropy = Some((bits, pw, image));
    }
    // Groups the image never names are read (they are in the stream) and
    // dropped; the used ones are numbered densely.
    let mut slot = vec![u32::MAX; num_groups];
    match &entropy {
        Some((_, _, image)) => {
            for &p in image {
                slot[((p >> 8) & 0xffff) as usize] = 0;
            }
            for (next, s) in slot.iter_mut().filter(|s| **s == 0).enumerate() {
                *s = next as u32;
            }
        }
        None => slot[0] = 0,
    }
    let alphabets = [NUM_LITERALS + NUM_LENGTH_CODES + cache_size, NUM_LITERALS, NUM_LITERALS, NUM_LITERALS, NUM_DISTANCE_CODES];
    let mut groups: Vec<Group> = Vec::new();
    for &s in &slot {
        let mut tables = Vec::with_capacity(5);
        for &size in &alphabets {
            let lengths = read_code_lengths(br, size)?;
            if br.overrun() {
                return Err(bitstream("VP8L data ends inside its prefix codes"));
            }
            if s != u32::MAX {
                tables.push(HuffmanTable::new(&lengths)?);
            }
        }
        if s != u32::MAX {
            let mut it = tables.into_iter();
            let (green, red, blue, alpha, distance) = (it.next().unwrap(), it.next().unwrap(), it.next().unwrap(), it.next().unwrap(), it.next().unwrap());
            let fixed_rba = match (red.single(), blue.single(), alpha.single()) {
                (Some(r), Some(b), Some(a)) => Some((u32::from(a) << 24) | (u32::from(r) << 16) | u32::from(b)),
                _ => None,
            };
            groups.push(Group { green, red, blue, alpha, distance, fixed_rba });
        }
    }
    let entropy = entropy.map(|(bits, pw, image)| (bits, pw, image.iter().map(|&p| slot[((p >> 8) & 0xffff) as usize] as usize).collect::<Vec<_>>()));

    let total = width * height;
    let mut px: Vec<u32> = Vec::with_capacity(total);
    let mut cache = vec![0u32; cache_size];
    let group_at = |x: usize, y: usize| -> usize {
        match &entropy {
            Some((bits, pw, image)) => image[(y >> bits) * pw + (x >> bits)],
            None => 0,
        }
    };
    let mask = match &entropy {
        Some((bits, _, _)) => (1usize << bits) - 1,
        None => usize::MAX,
    };
    let (mut x, mut y) = (0usize, 0usize);
    let mut g = &groups[group_at(0, 0)];
    // Pixels before this index are in the colour cache.
    let mut cached = 0usize;
    while px.len() < total {
        if x & mask == 0 {
            g = &groups[group_at(x, y)];
        }
        let s = g.green.read(br) as usize;
        if s < NUM_LITERALS {
            let argb = match g.fixed_rba {
                Some(rba) => rba | ((s as u32) << 8),
                None => {
                    let r = u32::from(g.red.read(br));
                    let b = u32::from(g.blue.read(br));
                    let a = u32::from(g.alpha.read(br));
                    (a << 24) | (r << 16) | ((s as u32) << 8) | b
                }
            };
            px.push(argb);
            x += 1;
            if x == width {
                x = 0;
                y += 1;
                if br.overrun() {
                    return Err(bitstream("VP8L data ends before the image does"));
                }
            }
        } else if s < NUM_LITERALS + NUM_LENGTH_CODES {
            let (base, extra) = prefix_base(s - NUM_LITERALS);
            let length = base + br.read(extra) as usize + 1;
            let dsym = g.distance.read(br) as usize;
            let (base, extra) = prefix_base(dsym);
            let code = base + br.read(extra) as usize + 1;
            let dist = code_to_distance(code, width);
            let pos = px.len();
            if dist > pos {
                return Err(bitstream("VP8L backward reference before the start of the image"));
            }
            if length > total - pos {
                return Err(bitstream("VP8L backward reference past the end of the image"));
            }
            if dist >= length {
                px.extend_from_within(pos - dist..pos - dist + length);
            } else {
                for k in 0..length {
                    let v = px[pos - dist + k];
                    px.push(v);
                }
            }
            x += length;
            while x >= width {
                x -= width;
                y += 1;
            }
            if br.overrun() {
                return Err(bitstream("VP8L data ends before the image does"));
            }
            if px.len() < total {
                g = &groups[group_at(x, y)];
            }
        } else {
            // The cache must be current up to here before it is read.
            if cache_size > 0 {
                for &p in &px[cached..] {
                    cache[cache_index(p, cache_bits)] = p;
                }
                cached = px.len();
            }
            let i = s - NUM_LITERALS - NUM_LENGTH_CODES;
            px.push(cache[i]);
            x += 1;
            if x == width {
                x = 0;
                y += 1;
                if br.overrun() {
                    return Err(bitstream("VP8L data ends before the image does"));
                }
            }
        }
    }
    if br.overrun() {
        return Err(bitstream("VP8L data ends before the image does"));
    }
    Ok(px)
}

/// Reads one prefix code's lengths for an alphabet of `size` symbols
/// (section 3.7.2.1).
fn read_code_lengths(br: &mut BitReader<'_>, size: usize) -> Result<Vec<u8>> {
    let mut lengths = vec![0u8; size];
    if br.read(1) == 1 {
        // Simple code length code: one or two symbols of length 1.
        let n = br.read(1) + 1;
        let first_bits = if br.read(1) == 1 { 8 } else { 1 };
        let s0 = br.read(first_bits) as usize;
        if s0 >= size {
            return Err(bitstream(format!("prefix code symbol {s0} outside an alphabet of {size}")));
        }
        lengths[s0] = 1;
        if n == 2 {
            let s1 = br.read(8) as usize;
            if s1 >= size {
                return Err(bitstream(format!("prefix code symbol {s1} outside an alphabet of {size}")));
            }
            lengths[s1] = 1;
        }
        return Ok(lengths);
    }
    // Normal code length code.
    let num = 4 + br.read(4) as usize;
    let mut cl_lengths = [0u8; 19];
    for &i in &CODE_LENGTH_ORDER[..num] {
        cl_lengths[i] = br.read(3) as u8;
    }
    let cl = HuffmanTable::new(&cl_lengths)?;
    let mut max_symbol = if br.read(1) == 1 {
        let nbits = 2 + 2 * br.read(3);
        let m = 2 + br.read(nbits) as usize;
        if m > size {
            return Err(bitstream(format!("max_symbol {m} above an alphabet of {size}")));
        }
        m
    } else {
        size
    };
    // max_symbol counts code length codes read (a repeat code is one),
    // not lengths filled (docs/PROVENANCE.md).
    let mut s = 0;
    let mut prev = 8u8;
    while s < size {
        if max_symbol == 0 {
            break;
        }
        max_symbol -= 1;
        let c = cl.read(br);
        if c < 16 {
            lengths[s] = c as u8;
            s += 1;
            if c != 0 {
                prev = c as u8;
            }
        } else {
            let (value, repeat) = match c {
                16 => (prev, 3 + br.read(2) as usize),
                17 => (0, 3 + br.read(3) as usize),
                _ => (0, 11 + br.read(7) as usize),
            };
            if s + repeat > size {
                return Err(bitstream("code length repeat runs past the alphabet"));
            }
            lengths[s..s + repeat].fill(value);
            s += repeat;
        }
        if br.overrun() {
            return Err(bitstream("VP8L data ends inside a prefix code"));
        }
    }
    Ok(lengths)
}
