#!/usr/bin/env python3
"""Fetch Google's public WebP test data, as data, for the conformance and
round-trip tests:

    python tools/fetch_testdata.py DIR

DIR receives

- `libwebp-test-data/`: the WebP files of Google's libwebp-test-data
  repository (https://chromium.googlesource.com/webm/libwebp-test-data, at a
  pinned commit), its `libwebp_tests.md5` (digests of libwebp's dwebp output
  for each file) and the reference renderings `grid.pam`, `peak.pam`,
  `lossless_color_transform.pam`, `grid.png`, `peak.png`;
- `gallery/`: the images of Google's WebP gallery
  (https://developers.google.com/speed/webp/gallery1 and gallery2), each
  WebP with the PNG Google publishes beside it, and the animated sample.

Every file is checked against tools/testdata.sha256; a file that does not
match is an error (the data has changed upstream; nothing is used unchecked).
Files already present with the right digest are not fetched again. About
30 MB. Then run

    WEBP_TESTDATA_DIR=DIR cargo test --release --test conformance -- --nocapture

None of this data is in the repository: it is Google's, and only fetched.
"""
import hashlib, io, os, sys, tarfile, time, urllib.request

# The repository at a pinned commit, as one archive (gitiles regenerates
# archives, so the archive's own bytes are not pinned; each file in it is).
TESTDATA = "https://chromium.googlesource.com/webm/libwebp-test-data/+archive/06ddd96e276c2c638a72d39d3c0f340afd61978c.tar.gz"
GSTATIC = "https://www.gstatic.com/webp/"


def fetch(url):
    for attempt in range(5):
        try:
            return urllib.request.urlopen(url).read()
        except urllib.error.HTTPError as e:
            if e.code != 429 or attempt == 4:
                raise
            time.sleep(5 * (attempt + 1))


_archive = None


def testdata_file(name):
    global _archive
    if _archive is None:
        print("fetching the libwebp-test-data archive")
        _archive = tarfile.open(fileobj=io.BytesIO(fetch(TESTDATA)), mode="r:gz")
    return _archive.extractfile(name).read()


def data_of(path):
    group, name = path.split("/", 1)
    if group == "libwebp-test-data":
        return testdata_file(name)
    # gallery/<dir>_<name>: gallery_1.webp -> gallery/1.webp,
    # gallery3_1_webp_a.png -> gallery3/1_webp_a.png, animated_1.webp ->
    # animated/1.webp.
    d, rest = name.split("_", 1)
    return fetch(GSTATIC + d + "/" + rest)


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    root = sys.argv[1]
    manifest = os.path.join(os.path.dirname(os.path.abspath(__file__)), "testdata.sha256")
    bad = 0
    for line in open(manifest):
        digest, path = line.split()
        dest = os.path.join(root, path)
        if os.path.exists(dest) and hashlib.sha256(open(dest, "rb").read()).hexdigest() == digest:
            continue
        data = data_of(path)
        got = hashlib.sha256(data).hexdigest()
        if got != digest:
            print("MISMATCH %s: %s, expected %s" % (path, got, digest))
            bad += 1
            continue
        os.makedirs(os.path.dirname(dest), exist_ok=True)
        with open(dest, "wb") as f:
            f.write(data)
        print("fetched", path)
    if bad:
        sys.exit("%d file(s) did not match their digests" % bad)
    print("all files present and verified")


if __name__ == "__main__":
    main()
