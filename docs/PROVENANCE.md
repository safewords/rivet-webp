# Provenance

Where every part of this crate came from. The short version: the code is
this repository's own, written from RFC 9649 (*WebP Image Format*, 2024) and
ITU-R BT.601; the lossy bitstream is handled by rivet-vp8, this project's own
VP8 codec written from RFC 6386; no WebP implementation was read or run.
Where the RFC is silent or ambiguous, the choice made is listed below with
what confirmed it — in every case that matters to the bitstream, Google's
public test files, used as data.

## Clean-room rules

- **No WebP implementation's source was opened or read**, nor searched for:
  not libwebp (its `src/`, its `examples/`, its `dwebp` / `cwebp`), not
  image-rs's `image-webp` or the `image` crate's WebP code, not
  libwebp-sys, not any other decoder or encoder. None was run either:
  there is no reference binary in the tests (black-box decoders were not
  needed, and none is used).
- RFC 9649 cites "the documentation in the libwebp source repository"
  (`doc/webp-lossless-bitstream-spec.txt`, `doc/webp-container-spec.txt`)
  as its origin. The RFC text itself is what was used; those documents
  were not read separately.
- The only external data is Google's public test material, fetched by
  `tools/fetch_testdata.py` and checked against `tools/testdata.sha256`,
  never committed: the libwebp-test-data repository (WebP files, the MD5
  digests of libwebp's decoded output, and a few reference renderings) and
  the WebP gallery (WebP files with the PNGs Google publishes beside them).
  Reading digests and reference pixels is using data, not code.
- rivet-png (this project's PNG codec) reads the gallery's PNGs and makes
  the PNG baselines in the tests; md5 hashes decoded output. Both are
  dev-dependencies only.

## The specification, by part

**RFC 9649 section 2** (container): the RIFF header and chunk layout with
padding, the simple lossy and lossless formats, `VP8X` and its flags,
`ANIM`, `ANMF` (frame offsets times two, 24-bit sizes and durations, the
blending and disposal bits), `ALPH` (preprocessing, filtering and
compression fields, the three filters and their edge rules), `ICCP`,
`EXIF`, `XMP `, unknown chunks, chunk order, and the canvas assembly
pseudocode (section 2.7.2) with the alpha-blending formula.

**RFC 9649 section 3** (VP8L): the header (signature, 14-bit sizes, alpha
hint, version), the four transforms (predictor with its 14 modes and border
rules, colour transform and `ColorTransformDelta`, subtract-green, colour
indexing with pixel bundling and delta-coded tables), the five roles of
image data, LZ77 prefix coding (the pseudocode) and the 120-entry distance
map (figure 20, transcribed and checked against the RFC's own examples:
code 1 is the pixel above, code 3 the top-left), the colour cache and its
hash multiplier, simple and normal code length codes with
`kCodeLengthCodeOrder`, repeat codes 16-18, `max_symbol`, meta prefix codes
and the entropy image, and the ABNF of section 3.8.

**RFC 6386** (VP8), through rivet-vp8: the `VP8 ` chunk is an RFC 6386 key
frame; this crate reads only its first ten bytes (frame tag, start code,
14-bit sizes) itself, to size the canvas before decoding.

**ITU-R BT.601**: the Y'CbCr ↔ R'G'B' matrices (Kr = 0.299, Kb = 0.114)
and the studio-range scaling (219 and 224 levels), from which the
fixed-point coefficients in `src/lossy.rs` were computed.

## Where the RFC is silent, ambiguous or self-contradictory

Each item says what this crate does and what confirms it.

### Lossless bitstream

1. **Prefix code construction.** The RFC says codes are canonical Huffman
   codes sent as lengths, and says nothing more: not the assignment order,
   not which bit of a code comes first in the LSB-first stream. This crate
   uses the DEFLATE convention — codes assigned by increasing length, then
   symbol; within the stream a code's most significant bit first.
   *Confirmed*: every lossless file in libwebp-test-data decodes to dwebp's
   output exactly; any other convention fails on the first multi-bit code.
2. **What `max_symbol` counts.** Section 3.7.2.1.2 says the table is "used
   to read up to max_symbol code lengths". This crate counts code length
   *symbols read* — a repeat code (16, 17, 18) counts once however many
   lengths it fills — not lengths filled. *Confirmed*: with the other
   reading, 20 of Google's files fail to decode (among them
   `bad_palette_index.webp`, `lossy_alpha2.webp`, `lossy_alpha3.webp`,
   six `alpha_filter_*` files and nine `lossless_vec_2_*`); with this one
   all decode exactly.
   The encoder never sends `max_symbol` (it always signals the full
   alphabet), so its output is unambiguous under either reading.
3. **Table 4 contradicts its pseudocode.** The row "3072..4096 → prefix 23,
   10 extra bits" disagrees with the RFC's own decoding pseudocode, under
   which prefix 23 is 3073..4096 and 3072 is prefix 22's last value (prefix
   22: offset 2 << 10 = 2048, values 2049..3072). The other rows agree with
   the pseudocode. This crate follows the pseudocode, which is what a
   decoder runs (`lossless::tests::prefix_coding_inverts`).
4. **Predictor modes 14 and 15.** The green channel names the mode; only 14
   modes exist and the RFC does not say what a larger value means. This
   crate takes the green channel's low four bits and predicts modes 14 and
   15 as mode 0 (opaque black). No test file uses them; the encoder never
   writes them.
5. **Codes with one used symbol** may be sent as a normal code length code
   with a single non-zero length; the RFC says that length is 1. A single
   symbol with any other length is accepted the same way (it reads no
   bits). A code with *no* non-zero length is refused as an incomplete tree
   (the RFC says an empty code is to be sent as one with the single symbol
   0); so is any incomplete or over-subscribed code.
6. **Symbols outside the alphabet.** A simple code's 8-bit symbol can name a
   symbol beyond a 40-symbol distance alphabet; a repeat code can run past
   the alphabet's end; `max_symbol` can exceed it. The RFC calls only the
   last invalid; this crate refuses all three.
7. **Backward references outside the image** (a distance reaching before
   the first pixel, a length running past the last) are not addressed by
   the RFC; they are errors here.
8. **Colour indexing with an index past the table** gives transparent black,
   as the RFC says it "should". *Confirmed*: `bad_palette_index.webp`
   decodes to dwebp's output.
9. **Unused meta prefix codes.** The number of prefix code groups is the
   entropy image's maximum plus one; groups the image never names are still
   in the stream. They are read (and checked) and not kept.
10. **`ClampAddSubtractHalf`** divides with C semantics, truncating toward
    zero for a negative difference; Rust's `/` does the same.
11. **The colour transform's red_to_blue term** uses the *original* red in
    the forward transform and the *restored* red in the inverse, which are
    the same value; the RFC's two code fragments say exactly that, though
    the prose could be read otherwise.

### Container and alpha

12. **ALPH header bit layout.** Diagrams number bits MSB-first (section
    2.2), so `Rsv|P|F|C` puts the compression method in the *low* two bits.
    *Confirmed* by the `alpha_filter_*` and `alpha_no_compression` files.
13. **ALPH image-stream.** "A compressed image-stream ... of implicit
    dimensions" is read as the ABNF's `image-stream` — transforms allowed —
    with the alpha in green. *Confirmed*: `alpha_color_cache.webp` and
    the lossy-with-alpha files decode to exact planes.
14. **Raw alpha longer than width x height**: the excess is ignored; shorter
    is an error.
15. **Level reduction** (`P = 1`) is informative; decoders "are not
    required to use this information". It is reported to no one and changes
    nothing.
16. **ALPH with a VP8L image** ("SHOULD NOT contain"): ignored. **ALPH
    without the VP8X alpha flag**: used anyway; the flag is a hint.
17. **Chunk order.** "Readers SHOULD fail when chunks necessary for
    reconstruction ... are out of order": this crate fails when `ICCP`
    comes after `ANIM` or the image data, `ANIM` after `ANMF`, `ANMF`
    before any `ANIM`, or `ALPH` after the frame's `VP8 `. Metadata and
    unknown chunks may be anywhere; duplicates of `ICCP`, `EXIF`, `XMP `,
    `ANIM`, `ALPH` and the bitstream are ignored after the first. `ANIM` and
    `ANMF` in a file whose animation flag is clear are ignored (the RFC says
    so for `ANIM`; `ANMF` "SHOULD NOT" be there).
18. **A simple-format file with chunks after its bitstream**: they are
    ignored.
19. **RIFF size and file length.** Data after the RIFF chunk is ignored (the
    RFC allows it). A RIFF size larger than the file is tolerated; a chunk
    that runs past the end of the data is an error; a missing final
    padding byte is tolerated.
20. **The still image's size** must equal the `VP8X` canvas; an `ANMF`
    frame's bitstream must equal the frame's declared size, and the frame
    must fit in the canvas. The RFC says these MUST hold; violations are
    errors.
21. **The VP8 frame header's scaling bits** (RFC 6386 section 9.2) are not
    mentioned by RFC 9649; they are ignored, as an upscaling hint for
    display.

### Animation

22. **Background colour.** A hint, the RFC says, which "MAY" fill the
    canvas. By default the canvas starts transparent black and disposal
    restores transparent black, which is how browsers show WebP
    animations; `DecodeOptions::use_background` uses the `ANIM` colour
    instead. `Info::background` reports it either way.
23. **Blending colour space.** The RFC says blending "SHOULD be done in
    linear color space". This crate blends the stored (gamma-encoded)
    values with the RFC's formula, in integers with rounding, exact for
    fully opaque and fully transparent sources. The encoder depends only on
    those exact cases.
24. **Loops.** `Decoder::frames` yields one pass; a viewer replays it
    `loop_count` times (0: forever), starting each pass from a fresh canvas
    as the RFC says.

### Lossy pictures

25. **Y'CbCr to RGB.** "Recommendation 601 SHOULD be used"; range, chroma
    siting and upsampling are left to the application. This crate uses
    studio range, chroma sited midway between luma samples, and bilinear
    upsampling (9/16, 3/16, 3/16, 1/16), in 14-bit fixed point. Against
    the PNG renderings Google publishes for its lossy gallery images, RGB
    differs by at most 2 levels (docs/VALIDATION.md); the decoded planes
    themselves are bit-exact with dwebp's.
26. **RGB to Y'CbCr** (encoder): the same matrix and range, chroma as the
    average of each 2x2 block, in 16-bit fixed point.

## The encoder

The lossless encoder is this crate's own design (no other encoder's
behaviour was studied): the transform strategy, the predictor and colour
transform searches, the hash-chain LZ77 with a cost-priced second pass, the
colour cache sizing, the clustering of meta prefix codes and the
length-limited Huffman construction are described in
`src/lossless/encode.rs` and `src/lossless/lz77.rs`. The quality-to-
quantiser curve of the lossy encoder (`127 (1 - q/100)^0.85`) is likewise
this crate's own; the VP8 encoding itself is rivet-vp8's.
