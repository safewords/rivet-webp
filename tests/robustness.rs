//! Malformed input is an error, never a panic, a hang or an allocation the
//! limits did not allow: files cut short at every length, bytes flipped at
//! random, headers that lie about sizes, and hand-made streams that break
//! one rule each. Run in debug too (`cargo test --test robustness`), where
//! arithmetic overflow panics.

use webp::{AnimationEncoder, AnimationOptions, DecodeOptions, Decoder, EncoderConfig, Error, Image, Limits};

fn image(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> Image {
    let mut rgba = Vec::new();
    for y in 0..h {
        for x in 0..w {
            rgba.extend_from_slice(&f(x, y));
        }
    }
    Image::new(w, h, rgba).unwrap()
}

/// Files that between them use every chunk and lossless feature the
/// encoder writes.
fn samples() -> Vec<(&'static str, Vec<u8>)> {
    let grad = image(48, 40, |x, y| [(x * 5) as u8, (y * 6) as u8, ((x * y) % 256) as u8, if (x + y) % 9 == 0 { 0 } else { 255 }]);
    let pal = image(37, 29, |x, y| [((x / 4 + y / 3) % 3 * 100) as u8, 50, 9, 255]);
    let big = image(130, 100, |x, y| {
        if x < 65 { [(x * 3) as u8, y as u8, 0, 255] } else { [((x * 7919 + y * 104_729) % 251) as u8, ((x ^ y) * 13) as u8, 77, 255] }
    });
    let meta = EncoderConfig {
        icc_profile: Some(vec![7; 33]),
        exif: Some(vec![1, 2, 3]),
        xmp: Some(b"<x/>".to_vec()),
        ..EncoderConfig::lossless()
    };
    let mut out = vec![
        ("lossless gradient+alpha", webp::encode(&grad, &EncoderConfig::lossless()).unwrap()),
        ("lossless palette", webp::encode(&pal, &EncoderConfig::lossless()).unwrap()),
        ("lossless meta codes", webp::encode(&big, &EncoderConfig { effort: 6, ..EncoderConfig::lossless() }).unwrap()),
        ("lossless with metadata", webp::encode(&grad, &meta).unwrap()),
        ("lossy with alpha", webp::encode(&grad, &EncoderConfig::lossy(60)).unwrap()),
        ("lossy opaque", webp::encode(&pal, &EncoderConfig::lossy(60)).unwrap()),
    ];
    for lossless in [true, false] {
        let mut enc = AnimationEncoder::new(48, 40, EncoderConfig { lossless, ..Default::default() }, AnimationOptions::default()).unwrap();
        for t in 0..3u32 {
            enc.add_frame(&image(48, 40, |x, y| if x / 8 == t { [255, 0, 0, 255] } else { [(x * 5) as u8, (y * 6) as u8, 40, 200] }), 50).unwrap();
        }
        out.push((if lossless { "lossless animation" } else { "lossy animation" }, enc.finish().unwrap()));
    }
    out
}

fn options() -> DecodeOptions {
    DecodeOptions {
        limits: Limits {
            max_pixels: 1 << 20,
            max_frames: 1000,
            max_animation_pixels: 1 << 24,
        },
        use_background: true,
    }
}

/// Everything a caller might do with a file; errors are fine.
fn exercise(data: &[u8]) {
    let Ok(dec) = Decoder::with_options(data, options()) else {
        return;
    };
    let _ = dec.info();
    let _ = dec.decode();
    for f in dec.frames() {
        if f.is_err() {
            break;
        }
    }
}

#[test]
fn samples_decode() {
    for (name, data) in samples() {
        let dec = Decoder::with_options(&data, options()).unwrap_or_else(|e| panic!("{name}: {e}"));
        dec.decode().unwrap_or_else(|e| panic!("{name}: {e}"));
        for f in dec.frames() {
            f.unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }
}

#[test]
fn every_truncation_is_an_error_or_a_picture() {
    let mut n = 0;
    for (_, data) in samples() {
        for len in 0..data.len() {
            exercise(&data[..len]);
            n += 1;
        }
    }
    println!("{n} truncated files");
}

#[test]
fn flipped_bytes_never_panic() {
    let mut s = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    for (_, data) in samples() {
        for _ in 0..1500 {
            let mut d = data.clone();
            let flips = 1 + (next() % 4) as usize;
            for _ in 0..flips {
                let i = (next() % d.len() as u64) as usize;
                d[i] ^= 1 << (next() % 8);
            }
            exercise(&d);
        }
        // Random bytes after the header, too.
        for _ in 0..300 {
            let mut d = data.clone();
            let start = 12 + (next() % (d.len() as u64 - 12)) as usize;
            for b in &mut d[start..] {
                *b = next() as u8;
            }
            exercise(&d);
        }
    }
}

fn riff(chunks: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
    let mut body = b"WEBP".to_vec();
    for (f, p) in chunks {
        body.extend_from_slice(*f);
        body.extend_from_slice(&(p.len() as u32).to_le_bytes());
        body.extend_from_slice(p);
        if p.len() % 2 == 1 {
            body.push(0);
        }
    }
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend(body);
    out
}

fn vp8l_header(w: u32, h: u32) -> Vec<u8> {
    let mut v = vec![0x2f];
    v.extend_from_slice(&((w - 1) | ((h - 1) << 14)).to_le_bytes());
    v
}

#[test]
fn sizes_over_the_limits_are_refused_before_decoding() {
    // A lossless header claiming 16384 x 16384, and nothing else.
    let file = riff(&[(b"VP8L", vp8l_header(16384, 16384))]);
    match Decoder::with_options(&file, options()) {
        Err(Error::LimitExceeded(_)) => {}
        other => panic!("expected a limit error, got {:?}", other.err()),
    }
    // A VP8X canvas of 2^24 x 2^24.
    let mut x = vec![0u8; 10];
    x[4..7].copy_from_slice(&[0xff, 0xff, 0xff]);
    x[7..10].copy_from_slice(&[0xff, 0xff, 0xff]);
    let file = riff(&[(b"VP8X", x), (b"VP8L", vp8l_header(1, 1))]);
    assert!(Decoder::new(&file).is_err());
    // Too many frames.
    let mut enc = AnimationEncoder::new(4, 4, EncoderConfig::lossless(), AnimationOptions::default()).unwrap();
    for i in 0..5u8 {
        enc.add_frame(&image(4, 4, |_, _| [i, 0, 0, 255]), 10).unwrap();
    }
    let file = enc.finish().unwrap();
    let opts = DecodeOptions { limits: Limits { max_frames: 3, ..Limits::default() }, ..Default::default() };
    assert!(matches!(Decoder::with_options(&file, opts), Err(Error::LimitExceeded(_))));
    let opts = DecodeOptions { limits: Limits { max_animation_pixels: 40, ..Limits::default() }, ..Default::default() };
    let dec = Decoder::with_options(&file, opts).unwrap();
    let results: Vec<_> = dec.frames().collect();
    assert_eq!(results.len(), 3);
    assert!(matches!(results[2], Err(Error::LimitExceeded(_))));
}

#[test]
fn broken_rules_are_errors() {
    let ok = webp::encode(&image(8, 8, |x, y| [x as u8 * 30, y as u8 * 30, 0, 255]), &EncoderConfig::lossless()).unwrap();
    let vp8l = ok[20..].to_vec();
    let bad = |what: &str, file: Vec<u8>| {
        let r = Decoder::new(&file).and_then(|d| d.decode());
        assert!(r.is_err(), "{what} was accepted");
    };
    bad("not RIFF", b"RIFX\x04\0\0\0WEBP".to_vec());
    bad("no chunks", riff(&[]));
    bad("unknown first chunk", riff(&[(b"ABCD", vec![1, 2])]));
    let mut v = vp8l.clone();
    v[0] = 0x2e;
    bad("bad VP8L signature", riff(&[(b"VP8L", v)]));
    let mut v = vp8l.clone();
    v[4] |= 0x20;
    bad("VP8L version 1", riff(&[(b"VP8L", v)]));
    // A chunk claiming more than the file holds.
    let mut f = riff(&[(b"VP8L", vp8l.clone())]);
    let n = f.len();
    f[16..20].copy_from_slice(&((n as u32) * 2).to_le_bytes());
    bad("chunk past the end", f);
    // VP8X still whose canvas differs from the bitstream.
    let x = {
        let mut x = vec![0u8; 10];
        x[4] = 9;
        x[7] = 7;
        x
    };
    bad("canvas mismatch", riff(&[(b"VP8X", x.clone()), (b"VP8L", vp8l.clone())]));
    // ICCP after the image data.
    let mut x8 = vec![0u8; 10];
    x8[0] = 0x20;
    x8[4] = 7;
    x8[7] = 7;
    bad("ICCP out of order", riff(&[(b"VP8X", x8.clone()), (b"VP8L", vp8l.clone()), (b"ICCP", vec![1])]));
    assert!(Decoder::new(&riff(&[(b"VP8X", x8.clone()), (b"ICCP", vec![1]), (b"VP8L", vp8l.clone())])).is_ok());
    // ALPH after VP8.
    let lossy = webp::encode(&image(8, 8, |_, _| [1, 2, 3, 255]), &EncoderConfig::lossy(50)).unwrap();
    let vp8 = lossy[20..].to_vec();
    let mut x9 = x8.clone();
    x9[0] = 0x10;
    bad("ALPH after VP8", riff(&[(b"VP8X", x9.clone()), (b"VP8 ", vp8.clone()), (b"ALPH", vec![0; 65])]));
    bad("ALPH compression 2", riff(&[(b"VP8X", x9.clone()), (b"ALPH", vec![2; 65]), (b"VP8 ", vp8.clone())]));
    bad("raw ALPH too short", riff(&[(b"VP8X", x9.clone()), (b"ALPH", vec![0; 10]), (b"VP8 ", vp8.clone())]));
    assert!(Decoder::new(&riff(&[(b"VP8X", x9.clone()), (b"ALPH", vec![0; 65]), (b"VP8 ", vp8.clone())])).unwrap().decode().is_ok());
    // An inter frame is not a WebP image.
    let mut inter = vp8.clone();
    inter[0] |= 1;
    bad("VP8 inter frame", riff(&[(b"VP8 ", inter)]));
    // Animation: frame outside the canvas, missing ANIM, no frames.
    let mut xa = x8.clone();
    xa[0] = 0x02;
    let mut anmf = vec![0u8; 16];
    anmf[0] = 1; // x = 2
    anmf[6] = 7; // width 8
    anmf[9] = 7;
    anmf.extend_from_slice(b"VP8L");
    anmf.extend_from_slice(&(vp8l.len() as u32).to_le_bytes());
    anmf.extend_from_slice(&vp8l);
    if vp8l.len() % 2 == 1 {
        anmf.push(0);
    }
    bad("frame outside the canvas", riff(&[(b"VP8X", xa.clone()), (b"ANIM", vec![0; 6]), (b"ANMF", anmf.clone())]));
    let mut inside = anmf.clone();
    inside[0] = 0;
    assert!(Decoder::new(&riff(&[(b"VP8X", xa.clone()), (b"ANIM", vec![0; 6]), (b"ANMF", inside.clone())])).unwrap().decode().is_ok());
    bad("ANMF without ANIM", riff(&[(b"VP8X", xa.clone()), (b"ANMF", inside.clone())]));
    bad("ANIM after ANMF", riff(&[(b"VP8X", xa.clone()), (b"ANIM", vec![0; 6]), (b"ANMF", inside.clone()), (b"ANIM", vec![0; 6])]));
    bad("animation without frames", riff(&[(b"VP8X", xa.clone()), (b"ANIM", vec![0; 6])]));
}

#[test]
fn hand_made_lossless_streams() {
    // 2x1 image; the bits after the header, LSB first.
    fn stream(bits: &[(u32, u32)]) -> Vec<u8> {
        let mut v = vp8l_header(2, 1);
        let (mut acc, mut n) = (0u64, 0);
        for &(val, len) in bits {
            acc |= u64::from(val) << n;
            n += len;
        }
        for _ in 0..n.div_ceil(8) + 4 {
            v.push(acc as u8);
            acc >>= 8;
        }
        riff(&[(b"VP8L", v)])
    }
    // No transform; colour cache with 12 bits: invalid.
    assert!(webp::decode(&stream(&[(0, 1), (1, 1), (12, 4)])).is_err());
    // Colour cache with 0 bits: invalid.
    assert!(webp::decode(&stream(&[(0, 1), (1, 1), (0, 4)])).is_err());
    // The same transform twice: subtract-green, subtract-green.
    assert!(webp::decode(&stream(&[(1, 1), (2, 2), (1, 1), (2, 2), (0, 1)])).is_err());
    // A valid stream: no transforms, no cache, no meta codes, five simple
    // one-symbol codes (green 7, red 1, blue 0, alpha 255 via 8 bits,
    // distance 0): two pixels 0xff010700, zero bits each.
    let simple = |sym: u32, eight: bool| -> Vec<(u32, u32)> {
        if eight { vec![(1, 1), (0, 1), (1, 1), (sym, 8)] } else { vec![(1, 1), (0, 1), (0, 1), (sym, 1)] }
    };
    let mut bits = vec![(0, 1), (0, 1), (0, 1)];
    bits.extend(simple(7, true));
    bits.extend(simple(1, false));
    bits.extend(simple(0, false));
    bits.extend(simple(255, true));
    bits.extend(simple(0, false));
    let img = webp::decode(&stream(&bits)).unwrap();
    assert_eq!(img.rgba, [1, 7, 0, 255, 1, 7, 0, 255]);
    // The same, but green's only symbol is a backward reference (length
    // code 0) at the first pixel: nothing to copy from.
    let mut bits = vec![(0, 1), (0, 1), (0, 1)];
    // green: normal code, a single length-1 symbol at 256.
    bits.extend([(0, 1), (0, 4)]); // normal, 4 code length code lengths
    // code length code: symbols 17, 18, 0, 1 -> lengths 0, 1, 0, 1:
    // tokens 18 and 1 each one bit.
    bits.extend([(0, 3), (1, 3), (0, 3), (1, 3)]);
    bits.push((0, 1)); // max_symbol = alphabet
    // 256 zeros (18 with 127 + 11 = 138, then 18 with 107 + 11 = 118), then 1.
    bits.extend([(1, 1), (127, 7), (1, 1), (107, 7), (0, 1)]);
    bits.extend(simple(0, false));
    bits.extend(simple(0, false));
    bits.extend(simple(0, false));
    bits.extend(simple(0, false));
    let r = webp::decode(&stream(&bits));
    assert!(r.is_err(), "{r:?}");
}

#[test]
fn encoder_refuses_bad_input() {
    assert!(Image::new(2, 2, vec![0; 15]).is_err());
    let img = image(2, 2, |_, _| [0, 0, 0, 255]);
    assert!(webp::encode(&img, &EncoderConfig { quality: 0, ..Default::default() }).is_err());
    assert!(webp::encode(&img, &EncoderConfig { effort: 7, ..Default::default() }).is_err());
    let wide = Image { width: 16385, height: 1, rgba: vec![255; 16385 * 4] };
    assert!(webp::encode(&wide, &EncoderConfig::lossless()).is_err());
    let mut enc = AnimationEncoder::new(2, 2, EncoderConfig::lossless(), AnimationOptions::default()).unwrap();
    assert!(enc.add_frame(&image(3, 2, |_, _| [0; 4]), 10).is_err());
    assert!(enc.add_frame(&img, 1 << 24).is_err());
    assert!(AnimationEncoder::new(2, 2, EncoderConfig::lossless(), AnimationOptions::default()).unwrap().finish().is_err());
}

/// The public test files, mutated, when the data is present.
#[test]
fn mutated_conformance_files() {
    let Some(dir) = std::env::var_os("WEBP_TESTDATA_DIR") else {
        eprintln!("skipped: set WEBP_TESTDATA_DIR (python tools/fetch_testdata.py DIR)");
        return;
    };
    let dir = std::path::PathBuf::from(dir).join("libwebp-test-data");
    let mut s = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    let mut names: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "webp")).collect();
    names.sort();
    for p in names {
        let data = std::fs::read(&p).unwrap();
        if data.len() > 200_000 {
            continue;
        }
        for _ in 0..60 {
            let mut d = data.clone();
            for _ in 0..1 + next() % 3 {
                let i = (next() % d.len() as u64) as usize;
                d[i] ^= 1 << (next() % 8);
            }
            exercise(&d);
        }
        for k in 1..8 {
            exercise(&data[..data.len() * k / 8]);
        }
    }
}
