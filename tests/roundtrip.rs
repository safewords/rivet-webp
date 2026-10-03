//! Encoder round trips: lossless must give back every byte (colour under
//! transparent pixels included, with `exact`), lossy must keep alpha
//! exactly and RGB close, animations must composite back to their frames,
//! metadata must come back as given.
//!
//! With `WEBP_TESTDATA_DIR` set (tools/fetch_testdata.py), the natural
//! images of the WebP gallery are round-tripped too and the lossless sizes
//! compared with PNG (rivet-png at its strongest setting); run with
//! `--nocapture` for the table.

use std::path::PathBuf;
use std::time::Instant;

use webp::{AnimationEncoder, AnimationOptions, Decoder, EncoderConfig, Image};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 16) as u32
    }
}

fn image(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> Image {
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            rgba.extend_from_slice(&f(x, y));
        }
    }
    Image::new(w, h, rgba).unwrap()
}

/// Synthetic pictures, each stressing something different.
fn synthetic() -> Vec<(&'static str, Image)> {
    let mut v = vec![
        ("1x1", image(1, 1, |_, _| [1, 2, 3, 4])),
        ("row 300x1", image(300, 1, |x, _| [x as u8, (x * 3) as u8, 9, 255])),
        ("column 1x300", image(1, 300, |_, y| [y as u8, 0, (y / 2) as u8, 255])),
        ("gradient", image(173, 91, |x, y| [(x * 255 / 172) as u8, (y * 255 / 90) as u8, ((x + y) / 2) as u8, 255])),
        ("gradient with smooth alpha", image(97, 61, |x, y| [x as u8, y as u8, 128, ((x * y) % 256) as u8])),
    ];
    let mut r = Rng(42);
    let n: Vec<u8> = (0..64 * 48 * 4).map(|_| r.next() as u8).collect();
    v.push(("noise", Image::new(64, 48, n).unwrap()));
    for colours in [1u32, 2, 3, 4, 5, 16, 17, 256, 257] {
        let name: &'static str = Box::leak(format!("{colours} colours").into_boxed_str());
        v.push((
            name,
            image(83, 37, move |x, y| {
                let k = ((x / 3) * 7 + (y / 2) * 13) % colours;
                [(k * 37) as u8, (k * 11 + (k >> 8)) as u8, (k * 3) as u8, if k % 5 == 0 { 128 } else { 255 }]
            }),
        ));
    }
    v.push((
        "binary alpha, colour under transparency",
        image(70, 50, |x, y| {
            let inside = (x as i32 - 35).pow(2) + (y as i32 - 25).pow(2) < 400;
            [(x * 3) as u8, (y * 5) as u8, (x ^ y) as u8, if inside { 255 } else { 0 }]
        }),
    ));
    v.push((
        "stripes and blocks (LZ77)",
        image(256, 128, |x, y| {
            if y % 16 < 8 { [((x / 8) * 40) as u8, 0, 200, 255] } else { [10, ((y / 16) * 30) as u8, (x % 4 * 60) as u8, 255] }
        }),
    ));
    v.push((
        "text-like",
        image(200, 60, |x, y| {
            let on = ((x / 2 + y / 3) % 7 == 0) || ((x * 3 + y) % 11 == 0 && y % 12 < 9);
            if on { [20, 20, 30, 255] } else { [250, 248, 240, 255] }
        }),
    ));
    v
}

#[test]
fn lossless_round_trips_exactly() {
    for (name, img) in synthetic() {
        for effort in 0..=6 {
            let cfg = EncoderConfig { effort, ..EncoderConfig::lossless() };
            let file = webp::encode(&img, &cfg).unwrap();
            let back = webp::decode(&file).unwrap();
            assert!(back == img, "{name}, effort {effort}");
            let info = webp::probe(&file).unwrap();
            assert_eq!(info.format, webp::Format::Lossless);
            assert!(!info.extended);
        }
    }
}

#[test]
fn lossless_not_exact_clears_only_invisible_colour() {
    for (name, img) in synthetic() {
        let cfg = EncoderConfig { exact: false, ..EncoderConfig::lossless() };
        let back = webp::decode(&webp::encode(&img, &cfg).unwrap()).unwrap();
        for (a, b) in img.rgba.as_chunks::<4>().0.iter().zip(back.rgba.as_chunks::<4>().0) {
            if a[3] == 0 {
                assert_eq!(b[3], 0, "{name}");
            } else {
                assert_eq!(a, b, "{name}");
            }
        }
    }
}

/// PSNR of the RGB of the pixels `a` shows (alpha above 0).
fn psnr(a: &Image, b: &Image) -> f64 {
    let (mut sq, mut n) = (0f64, 0f64);
    for (p, q) in a.rgba.as_chunks::<4>().0.iter().zip(b.rgba.as_chunks::<4>().0) {
        if p[3] == 0 {
            continue;
        }
        for c in 0..3 {
            let d = f64::from(p[c]) - f64::from(q[c]);
            sq += d * d;
            n += 1.0;
        }
    }
    if sq == 0.0 { 99.0 } else { 10.0 * (255.0 * 255.0 / (sq / n)).log10() }
}

#[test]
fn lossy_keeps_alpha_exactly() {
    for (name, img) in synthetic() {
        if img.width < 8 || img.height < 8 {
            continue;
        }
        for quality in [10u8, 50, 90] {
            let file = webp::encode(&img, &EncoderConfig::lossy(quality)).unwrap();
            let back = webp::decode(&file).unwrap();
            let info = webp::probe(&file).unwrap();
            assert_eq!(info.format, webp::Format::Lossy);
            assert_eq!(info.extended, img.has_alpha(), "{name}");
            for (a, b) in img.rgba.as_chunks::<4>().0.iter().zip(back.rgba.as_chunks::<4>().0) {
                assert_eq!(a[3], b[3], "{name}: alpha");
            }
        }
    }
    // Smooth content survives at high quality.
    let img = image(160, 120, |x, y| [((x + y) * 255 / 278) as u8, (x * 255 / 159) as u8, (255 - 2 * y) as u8, 255]);
    let back = webp::decode(&webp::encode(&img, &EncoderConfig::lossy(90)).unwrap()).unwrap();
    let p = psnr(&img, &back);
    assert!(p > 38.0, "gradient at quality 90: {p:.2} dB");
}

#[test]
fn metadata_comes_back() {
    let img = image(20, 10, |x, y| [x as u8, y as u8, 0, 255]);
    for lossless in [false, true] {
        let cfg = EncoderConfig {
            lossless,
            icc_profile: Some(vec![1, 2, 3]),
            exif: Some(b"Exif\0\0MM".to_vec()),
            xmp: Some(b"<x:xmpmeta/>".to_vec()),
            ..Default::default()
        };
        let file = webp::encode(&img, &cfg).unwrap();
        let info = webp::probe(&file).unwrap();
        assert!(info.extended);
        assert_eq!(info.icc_profile.as_deref(), Some(&[1u8, 2, 3][..]));
        assert_eq!(info.exif.as_deref(), Some(&b"Exif\0\0MM"[..]));
        assert_eq!(info.xmp.as_deref(), Some(&b"<x:xmpmeta/>"[..]));
        let back = webp::decode(&file).unwrap();
        assert_eq!((back.width, back.height), (20, 10));
        if lossless {
            assert_eq!(back, img);
        }
    }
}

/// Frames that move, appear, fade and stand still.
fn animation_frames() -> Vec<Image> {
    let (w, h) = (90, 70);
    (0..12)
        .map(|t| {
            image(w, h, move |x, y| {
                let bx = (t * 6) % (w - 20);
                if (bx..bx + 20).contains(&x) && (20..40).contains(&y) {
                    [255, (t * 20) as u8, 0, 255]
                } else if (6..9).contains(&t) && y > 55 {
                    // A translucent band for three frames.
                    [0, 0, 255, 100]
                } else {
                    [(x * 2) as u8, (y * 3) as u8, 60, 255]
                }
            })
        })
        .chain(std::iter::once(image(w, h, |x, y| [(x * 2) as u8, (y * 3) as u8, 60, 255])))
        .chain(std::iter::once(image(w, h, |x, y| [(x * 2) as u8, (y * 3) as u8, 60, 255])))
        .collect()
}

#[test]
fn lossless_animation_composites_back_to_its_frames() {
    let frames = animation_frames();
    for effort in [0u8, 4] {
        let cfg = EncoderConfig { effort, ..EncoderConfig::lossless() };
        let opts = AnimationOptions { loop_count: 3, background: [1, 2, 3, 4] };
        let mut enc = AnimationEncoder::new(90, 70, cfg, opts).unwrap();
        for (i, f) in frames.iter().enumerate() {
            enc.add_frame(f, 40 + i as u32).unwrap();
        }
        let file = enc.finish().unwrap();
        let dec = Decoder::new(&file).unwrap();
        let info = dec.info();
        assert!(info.animated);
        assert_eq!((info.frame_count, info.loop_count, info.background), (frames.len(), 3, [1, 2, 3, 4]));
        let mut t = 0;
        for (i, got) in dec.frames().enumerate() {
            let got = got.unwrap();
            assert!(got.image == frames[i], "frame {i}, effort {effort}");
            assert_eq!((got.duration_ms, got.timestamp_ms), (40 + i as u32, t));
            t += u64::from(got.duration_ms);
        }
        // The still is the first frame.
        assert_eq!(webp::decode(&file).unwrap(), frames[0]);
    }
}

#[test]
fn lossy_animation_keeps_alpha_and_shape() {
    let frames = animation_frames();
    let mut enc = AnimationEncoder::new(90, 70, EncoderConfig::lossy(85), AnimationOptions::default()).unwrap();
    for f in &frames {
        enc.add_frame(f, 100).unwrap();
    }
    let file = enc.finish().unwrap();
    let dec = Decoder::new(&file).unwrap();
    assert_eq!(dec.info().format, webp::Format::Lossy);
    for (i, got) in dec.frames().enumerate() {
        let got = got.unwrap().image;
        for (a, b) in frames[i].rgba.as_chunks::<4>().0.iter().zip(got.rgba.as_chunks::<4>().0) {
            assert_eq!(a[3], b[3], "frame {i}: alpha");
        }
        let p = psnr(&frames[i], &got);
        // Hard-edged synthetic shapes: ringing keeps this modest.
        assert!(p > 27.0, "frame {i}: {p:.2} dB");
    }
}

fn data_dir() -> Option<PathBuf> {
    std::env::var_os("WEBP_TESTDATA_DIR").map(PathBuf::from)
}

fn read_png(path: &std::path::Path) -> Image {
    let png = rpng::decode(&std::fs::read(path).unwrap()).unwrap();
    Image::new(png.image.width, png.image.height, png.image.to_rgba8()).unwrap()
}

fn png_size(img: &Image) -> usize {
    let alpha = img.has_alpha();
    let data: Vec<u8> = if alpha {
        img.rgba.clone()
    } else {
        img.rgba.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect()
    };
    let ct = if alpha { rpng::ColorType::Rgba } else { rpng::ColorType::Rgb };
    let png = rpng::Image::new(img.width, img.height, ct, 8, data).unwrap();
    rpng::Encoder { filter: rpng::FilterStrategy::Adaptive, ..rpng::Encoder::with_level(9) }.encode(&png).unwrap().len()
}

#[test]
fn natural_images() {
    let Some(dir) = data_dir() else {
        eprintln!("skipped: set WEBP_TESTDATA_DIR (python tools/fetch_testdata.py DIR)");
        return;
    };
    let mut files: Vec<PathBuf> = Vec::new();
    for i in 1..=5 {
        files.push(dir.join(format!("gallery/gallery3_{i}.png")));
    }
    for i in 1..=5 {
        files.push(dir.join(format!("gallery/gallery_{i}.png")));
    }
    files.push(dir.join("libwebp-test-data/peak.png"));
    files.push(dir.join("libwebp-test-data/grid.png"));
    println!("| image | size | alpha | PNG -9 bytes | WebP lossless e0 | e4 | e6 | e4 vs PNG | e4 encode ms | lossy q80 bytes | q80 PSNR dB |");
    println!("|---|---|---|---|---|---|---|---|---|---|---|");
    let (mut png_total, mut e4_total) = (0usize, 0usize);
    for f in files {
        let img = read_png(&f);
        let png = png_size(&img);
        let mut sizes = Vec::new();
        let mut ms = 0;
        for effort in [0u8, 4, 6] {
            let t = Instant::now();
            let file = webp::encode(&img, &EncoderConfig { effort, ..EncoderConfig::lossless() }).unwrap();
            if effort == 4 {
                ms = t.elapsed().as_millis();
            }
            let back = webp::decode(&file).unwrap();
            assert!(back == img, "{}: effort {effort} does not round-trip", f.display());
            sizes.push(file.len());
        }
        let lossy = webp::encode(&img, &EncoderConfig::lossy(80)).unwrap();
        let back = webp::decode(&lossy).unwrap();
        for (a, b) in img.rgba.as_chunks::<4>().0.iter().zip(back.rgba.as_chunks::<4>().0) {
            assert_eq!(a[3], b[3]);
        }
        png_total += png;
        e4_total += sizes[1];
        println!(
            "| {} | {}x{} | {} | {png} | {} | {} | {} | {:.1}% | {ms} | {} | {:.2} |",
            f.file_name().unwrap().to_string_lossy(),
            img.width,
            img.height,
            if img.has_alpha() { "yes" } else { "no" },
            sizes[0],
            sizes[1],
            sizes[2],
            100.0 * sizes[1] as f64 / png as f64,
            lossy.len(),
            psnr(&img, &back)
        );
    }
    println!("\nlossless effort 4 in total: {:.1}% of PNG -9 ({e4_total} / {png_total} bytes)", 100.0 * e4_total as f64 / png_total as f64);
}
