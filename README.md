# rivet-webp

[![CI](https://github.com/safewords/rivet-webp/actions/workflows/ci.yml/badge.svg)](https://github.com/safewords/rivet-webp/actions/workflows/ci.yml)

A **WebP decoder and encoder** in Rust: lossy and lossless stills, alpha,
animation, metadata. No C, no system libraries, no build script, nothing to
install on a build host. Written from RFC 9649 (*WebP Image Format*), not
translated from libwebp or any other implementation; lossy frames are VP8,
coded by [rivet-vp8](https://github.com/safewords/rivet-vp8), this
project's own clean-room VP8 codec. The decoder reproduces Google's
reference output on every file of Google's public WebP test data it was
checked on (the figures are [below](#how-it-is-checked)).

Written for the **[rivet](https://github.com/safewords/rivet)**
transcoder, where it replaces the `image` crate's WebP support and
libwebp. Usable on its own by anything that has WebP bytes and wants RGBA,
or RGBA and wants WebP.

Published as `rivet-webp`; **imported as `webp`** (`use webp::…`). One
dependency (rivet-vp8), no features, no build script, no `unsafe`.

```toml
[dependencies]
webp = { package = "rivet-webp", git = "https://github.com/safewords/rivet-webp", branch = "develop" }
```

## Use

```rust
// Decode: a still, or an animation's first frame.
let image = webp::decode(&bytes)?; // webp::Image { width, height, rgba }

// What the file holds, without decoding pixels.
let info = webp::probe(&bytes)?;   // size, alpha, animation, ICC / Exif / XMP

// Every frame of an animation, composited.
let dec = webp::Decoder::new(&bytes)?;
for frame in dec.frames() {
    let frame = frame?;            // whole canvas, duration, timestamp
}

// Encode: lossless, or lossy at a quality of 1-100.
let lossless = webp::encode(&image, &webp::EncoderConfig::lossless())?;
let lossy = webp::encode(&image, &webp::EncoderConfig {
    quality: 80,
    icc_profile: Some(icc),
    ..Default::default()
})?;

// Animations.
let mut enc = webp::AnimationEncoder::new(w, h, webp::EncoderConfig::lossless(), Default::default())?;
enc.add_frame(&frame_image, 100)?;
let file = enc.finish()?;
```

`examples/webptool.rs` is a small command-line front end (`info`, `decode`
to PAM, `time`, `encode` from PNG).

## What it decodes

| | supported |
|---|---|
| **Formats** | simple lossy (`VP8 `), simple lossless (`VP8L`), extended (`VP8X`) |
| **Lossless** | all four transforms — predictor (14 modes), colour transform, subtract-green, colour indexing with pixel bundling — colour cache (1-11 bits), meta prefix codes, LZ77 with the 120-code distance map |
| **Lossy** | every VP8 key frame (rivet-vp8, bit-exact on the VP8 test vectors); BT.601 conversion to RGB with bilinear chroma upsampling |
| **Alpha** | `ALPH` raw or lossless, all three filters; the preprocessing field is read and, being informative, left alone |
| **Animation** | `ANIM` / `ANMF`: frames composited to whole RGBA canvases with alpha-blending or overwrite, disposal to background, durations, loop count, background colour (as a hint: transparent by default, the `ANIM` colour on request) |
| **Metadata** | `ICCP`, `EXIF`, `XMP ` and unknown chunks handed back as they are |
| **Robustness** | malformed input is an error, never a panic; `Limits` bound pixels, frames and compositing work before anything is allocated |

Chunk order is checked as RFC 9649 asks (`ICCP`, `ANIM`, image data);
metadata and unknown chunks may be anywhere. Every point where the RFC is
silent or ambiguous, and what this crate does there, is listed in
[docs/PROVENANCE.md](docs/PROVENANCE.md).

## What it encodes

- **Lossless (VP8L)**, bit-exact, with this crate's own encoder: a palette
  (with pixel bundling) for pictures of up to 256 colours; otherwise
  subtract-green, a predictor chosen per 4x4 or 8x8 block by estimated
  bits, and a colour transform searched per block; hash-chain LZ77 with
  lazy matching and a second pass priced by the first one's statistics; a
  colour cache sized by estimated cost; meta prefix codes from clustered
  block histograms; length-limited Huffman codes. Effort 0 (fast) to 6
  (small) chooses how much of that is searched. About 27% smaller than PNG
  at effort 4 over Google's gallery images. `exact: false` lets fully
  transparent pixels lose their colour, which compresses better.
- **Lossy (VP8)** through rivet-vp8, at a quality of 1-100, with alpha in a
  lossless `ALPH` chunk (the best of the four filters, or raw), so alpha is
  always exact.
- **Animation**, lossless or lossy: each frame stores only the rectangle
  that changed, and, where every changed pixel is opaque, blends it with
  the unchanged pixels made transparent — whichever is smaller.
- **Metadata**: ICC profile, Exif and XMP, in an extended (`VP8X`) file;
  without metadata, the simple format whenever it can hold the picture.

## How it is checked

- **Google's libwebp-test-data** (`tests/conformance.rs`): all 9 lossless
  files decode to exactly dwebp's RGBA (by its published MD5s); all 87
  lossy files decode to exactly dwebp's Y'CbCr planes and alpha; the 32
  `lossless_vec` files (every combination of transforms) and
  `lossless_color_transform.webp` match their reference images.
- **Google's WebP gallery**: lossless images match the published PNG
  renderings exactly; lossy images with alpha match in alpha exactly and in
  RGB within 2 levels (the RGB conversion is the application's choice,
  RFC 9649 says); the animated sample composites all 100 frames.
- **Round trips** (`tests/roundtrip.rs`): 18 synthetic and 12 natural
  pictures, every effort, lossless bit-exact; lossy alpha exact; animations
  composite back to their frames exactly; metadata comes back as given.
- **Robustness** (`tests/robustness.rs`): every truncation and thousands
  of corruptions of encoder-made and Google's files, and hand-made streams
  that each break one rule, in debug (overflow-checked) and release.

The data is Google's and not in the repository:
`python tools/fetch_testdata.py DIR` fetches it (about 30 MB, every file
checked against `tools/testdata.sha256`), and
`WEBP_TESTDATA_DIR=DIR cargo test --release -- --nocapture` runs all of the
above with the tables. Figures are in [docs/VALIDATION.md](docs/VALIDATION.md).

## License

Open Encoding Attribution License v1.0 — a source-available (not OSI open-source)
license, royalty-free, with a commercial-attribution requirement. See
[LICENSE.md](LICENSE.md) and [NOTICE](NOTICE).
