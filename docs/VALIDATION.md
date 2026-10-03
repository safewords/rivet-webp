# Validation

Figures measured 2026-10-03 on Windows x86-64, Rust 1.99, release build.
The test data is Google's, fetched by `tools/fetch_testdata.py` (every file
checked against `tools/testdata.sha256`) and not in the repository; the
tests that use it skip without `WEBP_TESTDATA_DIR`.

## Decoder: libwebp-test-data

Google's libwebp-test-data repository (pinned commit `06ddd96e`) holds 131
WebP files and, in `libwebp_tests.md5`, the MD5 of libwebp's `dwebp`
output for 96 of them in several formats. Two are used
(`tests/conformance.rs`):

- **PAM**: the decoded RGBA. For lossless files this is fully specified, and
  must match.
- **PGM**: dwebp's dump of the decoded Y', Cb and Cr planes, followed by
  the alpha plane when there is one. For lossy files this is what the
  bitstream specifies (the RGB conversion is not; see below), and must
  match.

| | files | result |
|---|---|---|
| lossless (VP8L), RGBA against dwebp's PAM digest | 9 | **9 exact** |
| lossy (VP8, 18 with ALPH), Y'CbCr + alpha against dwebp's PGM digest | 87 | **87 exact** |
| `lossless_vec_1_*` against `grid.pam` (every combination of predictor, cross-colour, subtract-green and palette transforms) | 16 | **16 exact** |
| `lossless_vec_2_*` against `peak.pam` | 16 | **16 exact** |
| `lossless_color_transform.webp` against its `.pam` | 1 | **exact** |
| `bryce.webp` (11158 x 2156 lossy), `lossless_big_random_alpha.webp` (2048 x 2048), no reference | 2 | decode |

The lossless files cover all four transforms, pixel bundling at every
width, colour caches up to 11 bits (`color_cache_bits_11.webp`), palette
indices past the table (`bad_palette_index.webp`), meta prefix codes,
`max_symbol`, and `near_lossless_75.webp`. The lossy files cover the VP8
comprehensive vectors re-wrapped as WebP, odd sizes down to 1x1, segment
maps, extreme probabilities, and every ALPH filter with raw and lossless
alpha. Nine of the lossy files also match dwebp's RGBA exactly (flat or
grey content); the others differ only through the Y'CbCr conversion.

## Decoder: the WebP gallery

Google's gallery publishes PNGs beside its WebP files (`tests/conformance.rs`,
`gallery` test):

| file | size | reference | alpha | RGB max diff | RGB mean abs diff | RGB PSNR dB |
|---|---|---|---|---|---|---|
| gallery3_1_webp_ll.webp | 400x301 | exact | exact | 0 | 0.000 | inf |
| gallery3_2_webp_ll.webp | 386x395 | exact | exact | 0 | 0.000 | inf |
| gallery3_3_webp_ll.webp | 800x600 | exact | exact | 0 | 0.000 | inf |
| gallery3_4_webp_ll.webp | 421x163 | exact | exact | 0 | 0.000 | inf |
| gallery3_5_webp_ll.webp | 300x300 | exact | exact | 0 | 0.000 | inf |
| gallery3_1_webp_a.webp | 400x301 | tolerance | exact | 2 | 0.342 | 52.79 |
| gallery3_2_webp_a.webp | 386x395 | tolerance | exact | 2 | 0.078 | 59.20 |
| gallery3_3_webp_a.webp | 800x600 | tolerance | exact | 2 | 0.288 | 53.53 |
| gallery3_4_webp_a.webp | 421x163 | tolerance | exact | 2 | 0.230 | 54.50 |
| gallery3_5_webp_a.webp | 300x300 | tolerance | exact | 2 | 0.253 | 54.10 |

- Lossless images match their renderings exactly, alpha included.
- Lossy images with alpha: **alpha exact**; RGB within **2 levels** of
  Google's rendering everywhere, mean difference 0.08-0.34, PSNR 52.8-59.2
  dB. The decoded planes are bit-exact (above), so the difference is the
  Y'CbCr-to-RGB step, which RFC 9649 leaves to the application
  (docs/PROVENANCE.md, item 25). The test's tolerance is mean difference
  1.5 and PSNR 38 dB; colour under fully transparent pixels is not
  compared.
- The animated sample (`animated/1.webp`, 300x225, 100 lossy frames with
  alpha, 10 s, looping forever) composites all 100 frames; durations sum to
  the file's.

## Encoder: lossless round trips

`tests/roundtrip.rs` encodes 18 synthetic pictures — 1x1, a 300x1 row and a
1x300 column, gradients with and without smooth alpha, noise, 1, 2, 3, 4,
5, 16, 17, 256 and 257 colours (every bundling width and both sides of the
palette limit), binary alpha with colour under the transparent pixels,
stripes and blocks for LZ77, text-like strokes — at every effort 0-6, and
twelve natural images (the gallery's ten and libwebp-test-data's two PNGs)
at efforts 0, 4 and 6. **Every one decodes back bit-exact**, colour under
transparent pixels included. With `exact: false`, only the colour of fully
transparent pixels changes. The encoder's unit tests round-trip the same
way through the headerless (ALPH) form.

## Encoder: compression against PNG

Lossless sizes against PNG written by rivet-png at level 9 with adaptive
filtering (RGB, or RGBA where there is transparency); `e4` is the default
effort. Encode times are single-threaded.

| image | size | alpha | PNG -9 | WebP e0 | e4 | e6 | e4 / PNG | e4 ms |
|---|---|---|---|---|---|---|---|---|
| gallery3_1.png | 400x301 | yes | 124358 | 99160 | 90010 | 89154 | 72.4% | 27 |
| gallery3_2.png | 386x395 | yes | 44605 | 42920 | 31344 | 30692 | 70.3% | 43 |
| gallery3_3.png | 800x600 | yes | 241159 | 201414 | 163944 | 161486 | 68.0% | 81 |
| gallery3_4.png | 421x163 | yes | 52856 | 40610 | 37410 | 37138 | 70.8% | 19 |
| gallery3_5.png | 300x300 | yes | 139109 | 135922 | 103262 | 102970 | 74.2% | 39 |
| gallery_1.png | 550x368 | no | 385320 | 349102 | 309494 | 306850 | 80.3% | 67 |
| gallery_2.png | 550x404 | no | 553971 | 442780 | 416928 | 415084 | 75.3% | 61 |
| gallery_3.png | 1280x720 | no | 1974612 | 1337318 | 1229418 | 1218456 | 62.3% | 476 |
| gallery_4.png | 1024x772 | no | 1743807 | 1540862 | 1404942 | 1404248 | 80.6% | 409 |
| gallery_5.png | 1024x752 | no | 1198446 | 1096708 | 938158 | 935098 | 78.3% | 431 |
| peak.png | 128x128 | no | 26098 | 13208 | 10368 | 10360 | 39.7% | 14 |
| grid.png | 16x16 | yes | 90 | 44 | 46 | 46 | 51.1% | 2 |

In total, effort 4 is **73.0% of PNG** (4 735 324 against 6 484 431 bytes);
effort 0 is 81.7%, effort 6 72.7%. Effort 4 encodes about 2 to 2.5
megapixels a second on these photographs, effort 0 about 25.
The gallery_* images are JPEG-derived photographs, where no lossless coder
does much; the gallery3 graphics with alpha come out 27-32% smaller than
PNG.

For a sense of distance from libwebp's own encoder, the gallery publishes
its lossless encodings of the five gallery3 images: 81 836, 27 650, 152 614,
33 986 and 99 434 bytes. This encoder at effort 6 with `exact: false` (as
those files evidently were made: the published renderings drop the colour
of transparent pixels) gives 88 044, 30 658, 161 224, 34 872 and 103 256 —
3-12% larger.

## Encoder: lossy

The lossy path converts to Y'CbCr and hands the frame to rivet-vp8's
encoder at a quantiser derived from the quality (`127 (1 - q/100)^0.85`;
quality 80 is quantiser 32). Alpha goes in a lossless ALPH chunk, so it is
**always exact** (`lossy_keeps_alpha_exactly`, and every natural image
above). At quality 80, against the source PNG:

| image | bytes | PSNR dB (visible pixels) |
|---|---|---|
| gallery_1 | 27 294 | 31.66 |
| gallery_2 | 65 528 | 30.24 |
| gallery_3 | 214 300 | 31.04 |
| gallery_4 | 189 004 | 30.19 |
| gallery_5 | 63 104 | 32.10 |

Google's gallery WebP files of the same images measure 30 320 B / 31.13 dB,
60 600 / 29.19, 203 138 / 30.53, 176 972 / 29.62 and 82 698 / 32.99 against
the same PNGs (decoded by this crate): rate and quality of the same order.

## Animation

`lossless_animation_composites_back_to_its_frames`: 14 frames (a moving
block, a translucent band for three frames, two identical frames at the
end), encoded at efforts 0 and 4 with sub-rectangles, blending and the
transparent-pixel trick, decode back to **exactly** the input canvases,
with their durations and timestamps, loop count and background.
`lossy_animation_keeps_alpha_and_shape`: the same frames lossy keep alpha
exactly and RGB above 27 dB on hard-edged synthetic content.

## Robustness

`tests/robustness.rs` (run in debug as well, where integer overflow
panics):

- every truncation of eight encoder-made files covering every chunk and
  lossless feature the encoder writes (6 064 files);
- 14 400 corruptions of the same files (1-4 flipped bits, or random bytes
  from a random point on);
- every libwebp-test-data file under 200 KB, 60 random corruptions and 7
  truncations each;
- hand-made streams breaking one rule each (bad signature, version 1,
  colour cache of 0 or 12 bits, a transform used twice, a backward
  reference before the first pixel, chunks out of order, frames outside
  the canvas, ALPH method 2, an inter frame in a `VP8 ` chunk, ...);
- sizes over the limits refused before anything is allocated (a 16384 x
  16384 header in 28 bytes, a 2^24 x 2^24 canvas, too many frames, too much
  compositing).

No input panics; each is an `Error` or a picture.

## Speed

Decoding, release build, one thread: 49.5 Mpx/s for `bryce.webp` (24 Mpx
lossy), 51 Mpx/s for a 2048x2048 lossless image with random alpha, 88 Mpx/s
for gallery3_3's lossless file, 83 Mpx/s compositing the 100-frame
animation.
