//! Google's public WebP test files, decoded and checked against Google's own
//! reference outputs:
//!
//! - **libwebp-test-data**: `libwebp_tests.md5` holds the MD5 of libwebp's
//!   `dwebp` output for each file as PAM (RGBA) and PGM (the decoded
//!   Y'CbCr planes, then alpha). Lossless files must match the PAM digest
//!   exactly. Lossy files must match the PGM digest exactly (the VP8 planes
//!   and the alpha plane bit for bit); their RGB depends on each decoder's
//!   Y'CbCr conversion, which RFC 9649 leaves open, and is compared with
//!   reference renderings in `gallery` below. The `lossless_vec_*` files
//!   must decode to `grid.pam` / `peak.pam`, and
//!   `lossless_color_transform.webp` to its `.pam`.
//! - **The WebP gallery**: lossless files against the PNG renderings Google
//!   publishes beside them (exact); lossy files with alpha against their
//!   PNG renderings (alpha exact, RGB within a documented tolerance); the
//!   animated sample composited frame by frame.
//!
//! The data is not in the repository: `python tools/fetch_testdata.py DIR`
//! fetches it (checking every file's SHA-256), and
//! `WEBP_TESTDATA_DIR=DIR cargo test --release --test conformance -- --nocapture`
//! runs this. Without the variable the test reports that it skipped.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn data_dir() -> Option<PathBuf> {
    let d = std::env::var_os("WEBP_TESTDATA_DIR")?;
    Some(PathBuf::from(d))
}

fn pam(img: &webp::Image) -> Vec<u8> {
    let mut out = format!("P7\nWIDTH {}\nHEIGHT {}\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n", img.width, img.height).into_bytes();
    out.extend_from_slice(&img.rgba);
    out
}

/// The RGBA of a PAM file.
fn read_pam(path: &Path) -> webp::Image {
    let b = std::fs::read(path).unwrap();
    let end = b.windows(7).position(|w| w == b"ENDHDR\n").unwrap() + 7;
    let head = std::str::from_utf8(&b[..end]).unwrap();
    let field = |k: &str| -> u32 { head.lines().find_map(|l| l.strip_prefix(k)).unwrap().trim().parse().unwrap() };
    let (w, h) = (field("WIDTH"), field("HEIGHT"));
    webp::Image::new(w, h, b[end..].to_vec()).unwrap()
}

/// A WebP file's top-level chunks (and an ANMF's), by FourCC.
fn chunks(data: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut out = Vec::new();
    let mut at = 12;
    while at + 8 <= data.len() {
        let size = u32::from_le_bytes(data[at + 4..at + 8].try_into().unwrap()) as usize;
        let end = (at + 8 + size).min(data.len());
        out.push((data[at..at + 4].try_into().unwrap(), &data[at + 8..end]));
        at += 8 + size + (size & 1);
    }
    out
}

/// dwebp's PGM: Y rows, then rows of U beside V, then alpha rows if any.
fn pgm(vp8_payload: &[u8], alpha: Option<&[u8]>) -> Vec<u8> {
    let f = vp8::Decoder::new().decode(vp8_payload).unwrap().unwrap();
    let (w, h) = (f.width as usize, f.height as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let width = 2 * cw;
    let height = h + ch + if alpha.is_some() { h } else { 0 };
    let mut out = format!("P5\n{width} {height}\n255\n").into_bytes();
    let (y, u, v) = (f.plane(0), f.plane(1), f.plane(2));
    for r in 0..h {
        out.extend_from_slice(&y[r * w..(r + 1) * w]);
        out.resize(out.len() + width - w, 0);
    }
    for r in 0..ch {
        out.extend_from_slice(&u[r * cw..(r + 1) * cw]);
        out.extend_from_slice(&v[r * cw..(r + 1) * cw]);
    }
    if let Some(a) = alpha {
        for r in 0..h {
            out.extend_from_slice(&a[r * w..(r + 1) * w]);
            out.resize(out.len() + width - w, 0);
        }
    }
    out
}

fn md5_hex(b: &[u8]) -> String {
    format!("{:x}", md5::compute(b))
}

#[test]
fn libwebp_test_data() {
    let Some(dir) = data_dir() else {
        eprintln!("skipped: set WEBP_TESTDATA_DIR (python tools/fetch_testdata.py DIR)");
        return;
    };
    let dir = dir.join("libwebp-test-data");
    let list = std::fs::read_to_string(dir.join("libwebp_tests.md5")).unwrap();
    let mut digests: HashMap<String, String> = HashMap::new();
    for line in list.lines() {
        if let Some((hash, name)) = line.split_once("  ") {
            digests.insert(name.trim().to_string(), hash.to_string());
        }
    }
    let mut names: Vec<String> = digests.keys().filter_map(|k| k.strip_suffix(".pam").map(str::to_string)).collect();
    names.sort();
    let mut failures = Vec::new();
    let (mut exact_rgba, mut exact_yuv) = (0, 0);
    println!("| file | format | size | PAM (RGBA) | PGM (Y'CbCr + alpha) |");
    println!("|---|---|---|---|---|");
    for name in &names {
        let data = std::fs::read(dir.join(name)).unwrap();
        let info = match webp::probe(&data) {
            Ok(i) => i,
            Err(e) => {
                failures.push(format!("{name}: {e}"));
                continue;
            }
        };
        let img = match webp::decode(&data) {
            Ok(i) => i,
            Err(e) => {
                failures.push(format!("{name}: {e}"));
                continue;
            }
        };
        let pam_ok = md5_hex(&pam(&img)) == digests[&format!("{name}.pam")];
        let lossy = info.format == webp::Format::Lossy;
        let pgm_ok = if lossy {
            let c = chunks(&data);
            let vp8 = c.iter().find(|(f, _)| f == b"VP8 ").unwrap().1;
            let has_alph = c.iter().any(|(f, _)| f == b"ALPH");
            let a: Option<Vec<u8>> = has_alph.then(|| img.rgba.as_chunks::<4>().0.iter().map(|p| p[3]).collect());
            let got = md5_hex(&pgm(vp8, a.as_deref()));
            Some(got == digests[&format!("{name}.pgm")])
        } else {
            None
        };
        if pam_ok {
            exact_rgba += 1;
        }
        if pgm_ok == Some(true) {
            exact_yuv += 1;
        }
        println!(
            "| {name} | {} | {}x{} | {} | {} |",
            if lossy { if info.has_alpha { "lossy+alpha" } else { "lossy" } } else { "lossless" },
            img.width,
            img.height,
            if pam_ok { "exact" } else { "differs" },
            match pgm_ok {
                Some(true) => "exact",
                Some(false) => "DIFFERS",
                None => "-",
            }
        );
        if lossy && pgm_ok != Some(true) {
            failures.push(format!("{name}: decoded planes differ from dwebp's"));
        }
        if !lossy && !pam_ok {
            failures.push(format!("{name}: lossless RGBA differs from dwebp's"));
        }
    }
    println!(
        "\n{} files: {exact_rgba} RGBA-exact, {exact_yuv} of the lossy ones plane-exact",
        names.len()
    );

    // The lossless vectors, against their stated renderings.
    let grid = read_pam(&dir.join("grid.pam"));
    let peak = read_pam(&dir.join("peak.pam"));
    let mut vecs = 0;
    for set in 1..=2 {
        for i in 0..16 {
            let name = format!("lossless_vec_{set}_{i}.webp");
            let data = std::fs::read(dir.join(&name)).unwrap();
            let want = if set == 1 { &grid } else { &peak };
            match webp::decode(&data) {
                Ok(img) if &img == want => vecs += 1,
                Ok(_) => failures.push(format!("{name}: differs from its reference")),
                Err(e) => failures.push(format!("{name}: {e}")),
            }
        }
    }
    println!("lossless_vec: {vecs} of 32 exact");
    let data = std::fs::read(dir.join("lossless_color_transform.webp")).unwrap();
    let want = read_pam(&dir.join("lossless_color_transform.pam"));
    match webp::decode(&data) {
        Ok(img) if img == want => println!("lossless_color_transform: exact"),
        Ok(_) => failures.push("lossless_color_transform: differs".into()),
        Err(e) => failures.push(format!("lossless_color_transform: {e}")),
    }
    // No reference: they must decode.
    for name in ["bryce.webp", "lossless_big_random_alpha.webp"] {
        let data = std::fs::read(dir.join(name)).unwrap();
        match webp::decode(&data) {
            Ok(img) => println!("{name}: decodes, {}x{}", img.width, img.height),
            Err(e) => failures.push(format!("{name}: {e}")),
        }
    }
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
}

fn read_png(path: &Path) -> webp::Image {
    let png = rpng::decode(&std::fs::read(path).unwrap()).unwrap();
    webp::Image::new(png.image.width, png.image.height, png.image.to_rgba8()).unwrap()
}

#[test]
fn gallery() {
    let Some(dir) = data_dir() else {
        eprintln!("skipped: set WEBP_TESTDATA_DIR (python tools/fetch_testdata.py DIR)");
        return;
    };
    let dir = dir.join("gallery");
    let mut failures = Vec::new();
    println!("| file | size | reference | alpha | RGB: max diff | RGB: mean abs diff | RGB PSNR dB |");
    println!("|---|---|---|---|---|---|---|");
    let compare = |name: &str, got: &webp::Image, want: &webp::Image, exact: bool, failures: &mut Vec<String>| {
        if (got.width, got.height) != (want.width, want.height) {
            failures.push(format!("{name}: {}x{} against a {}x{} reference", got.width, got.height, want.width, want.height));
            return;
        }
        let (mut max, mut sum, mut sq, mut n, mut alpha_diff) = (0i32, 0u64, 0u64, 0u64, 0usize);
        for (g, w) in got.rgba.as_chunks::<4>().0.iter().zip(want.rgba.as_chunks::<4>().0.iter()) {
            if g[3] != w[3] {
                alpha_diff += 1;
            }
            // Colour under a fully transparent pixel is not part of the
            // picture.
            if w[3] == 0 {
                continue;
            }
            for c in 0..3 {
                let d = (i32::from(g[c]) - i32::from(w[c])).abs();
                max = max.max(d);
                sum += d as u64;
                sq += (d * d) as u64;
                n += 1;
            }
        }
        let mean = sum as f64 / n.max(1) as f64;
        let psnr = if sq == 0 { f64::INFINITY } else { 10.0 * (255.0f64 * 255.0 / (sq as f64 / n as f64)).log10() };
        println!(
            "| {name} | {}x{} | {} | {} | {max} | {mean:.3} | {} |",
            got.width,
            got.height,
            if exact { "exact" } else { "tolerance" },
            if alpha_diff == 0 { "exact".to_string() } else { format!("{alpha_diff} differ") },
            if psnr.is_finite() { format!("{psnr:.2}") } else { "inf".into() }
        );
        if alpha_diff > 0 {
            failures.push(format!("{name}: {alpha_diff} alpha values differ"));
        }
        if exact && max > 0 {
            failures.push(format!("{name}: RGB differs (max {max})"));
        }
        // Tolerance for lossy RGB against another decoder's rendering:
        // the conversion and upsampling differ, the planes do not.
        if !exact && (mean > 1.5 || psnr < 38.0) {
            failures.push(format!("{name}: RGB outside tolerance (mean {mean:.3}, PSNR {psnr:.2})"));
        }
    };
    for i in 1..=5 {
        // Lossless: the PNG rendering must match exactly.
        let ll = format!("gallery3_{i}_webp_ll.webp");
        let got = webp::decode(&std::fs::read(dir.join(&ll)).unwrap());
        match got {
            Ok(img) => compare(&ll, &img, &read_png(&dir.join(format!("gallery3_{i}_webp_ll.png"))), true, &mut failures),
            Err(e) => failures.push(format!("{ll}: {e}")),
        }
        // Lossy with alpha: alpha exact, RGB within tolerance.
        let a = format!("gallery3_{i}_webp_a.webp");
        match webp::decode(&std::fs::read(dir.join(&a)).unwrap()) {
            Ok(img) => compare(&a, &img, &read_png(&dir.join(format!("gallery3_{i}_webp_a.png"))), false, &mut failures),
            Err(e) => failures.push(format!("{a}: {e}")),
        }
    }
    for i in 1..=5 {
        // Lossy: the published PNG is the source the WebP was made from,
        // not a rendering of it; this measures the codec, not the decoder.
        let name = format!("gallery_{i}.webp");
        match webp::decode(&std::fs::read(dir.join(&name)).unwrap()) {
            Ok(img) => {
                let src = read_png(&dir.join(format!("gallery_{i}.png")));
                let (mut sq, mut n) = (0u64, 0u64);
                for (g, w) in img.rgba.as_chunks::<4>().0.iter().zip(src.rgba.as_chunks::<4>().0.iter()) {
                    for c in 0..3 {
                        let d = i64::from(g[c]) - i64::from(w[c]);
                        sq += (d * d) as u64;
                        n += 1;
                    }
                }
                let psnr = 10.0 * (255.0f64 * 255.0 / (sq as f64 / n as f64)).log10();
                println!("| {name} | {}x{} | source PNG | - | - | - | {psnr:.2} |", img.width, img.height);
            }
            Err(e) => failures.push(format!("{name}: {e}")),
        }
    }
    // The animated sample: every frame composites, durations add up.
    let data = std::fs::read(dir.join("animated_1.webp")).unwrap();
    let dec = webp::Decoder::new(&data).unwrap();
    let info = dec.info();
    let mut count = 0;
    let mut last_ts = 0;
    for f in dec.frames() {
        match f {
            Ok(f) => {
                count += 1;
                last_ts = f.timestamp_ms + u64::from(f.duration_ms);
            }
            Err(e) => {
                failures.push(format!("animated_1.webp frame {count}: {e}"));
                break;
            }
        }
    }
    println!(
        "\nanimated_1.webp: {}x{}, {count} of {} frames composited, {last_ts} ms, loop count {}, background {:?}, {:?}",
        info.width, info.height, info.frame_count, info.loop_count, info.background, info.format
    );
    if count != info.frame_count || last_ts != info.duration_ms {
        failures.push("animated_1.webp: frames or durations do not add up".into());
    }
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
}
