//! A small command-line tool for trying the crate out:
//!
//! - `webptool info FILE`: what the file holds;
//! - `webptool decode FILE OUT.pam`: the picture (first frame) as PAM;
//! - `webptool time FILE`: decode every frame, timed;
//! - `webptool encode IN.png OUT.webp [lossless|qN] [eN] [inexact]`: encode
//!   a PNG (read with rivet-png).

use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: webptool info|decode|time FILE [OUT.pam]");
        std::process::exit(2);
    }
    let data = std::fs::read(&args[2]).expect("read");
    match args[1].as_str() {
        "info" => match webp::probe(&data) {
            Ok(i) => println!(
                "{}x{} {:?} alpha={} animated={} frames={} loop={} duration={}ms extended={} icc={:?} exif={:?} xmp={:?} unknown={}",
                i.width,
                i.height,
                i.format,
                i.has_alpha,
                i.animated,
                i.frame_count,
                i.loop_count,
                i.duration_ms,
                i.extended,
                i.icc_profile.as_ref().map(Vec::len),
                i.exif.as_ref().map(Vec::len),
                i.xmp.as_ref().map(Vec::len),
                i.unknown_chunks.len()
            ),
            Err(e) => println!("error: {e}"),
        },
        "decode" => {
            let img = webp::decode(&data).expect("decode");
            let mut out = format!("P7\nWIDTH {}\nHEIGHT {}\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n", img.width, img.height).into_bytes();
            out.extend_from_slice(&img.rgba);
            std::fs::write(&args[3], out).expect("write");
        }
        "time" => {
            let t = Instant::now();
            let dec = webp::Decoder::new(&data).expect("container");
            let mut n = 0;
            let mut px = 0u64;
            for f in dec.frames() {
                let f = f.expect("decode");
                n += 1;
                px += u64::from(f.image.width) * u64::from(f.image.height);
            }
            let s = t.elapsed().as_secs_f64();
            println!("{n} frame(s), {:.1} Mpx in {:.1} ms ({:.1} Mpx/s)", px as f64 / 1e6, s * 1e3, px as f64 / 1e6 / s);
        }
        "encode" => {
            let png = rpng::decode(&data).expect("PNG");
            let img = webp::Image::new(png.image.width, png.image.height, png.image.to_rgba8()).expect("image");
            let mut cfg = webp::EncoderConfig::default();
            for a in &args[4..] {
                if a == "lossless" {
                    cfg.lossless = true;
                } else if a == "inexact" {
                    cfg.exact = false;
                } else if let Some(q) = a.strip_prefix('q') {
                    cfg.quality = q.parse().expect("quality");
                } else if let Some(e) = a.strip_prefix('e') {
                    cfg.effort = e.parse().expect("effort");
                }
            }
            let t = Instant::now();
            let out = webp::encode(&img, &cfg).expect("encode");
            println!("{} bytes in {:.1} ms", out.len(), t.elapsed().as_secs_f64() * 1e3);
            std::fs::write(&args[3], out).expect("write");
        }
        other => eprintln!("unknown command {other}"),
    }
}
