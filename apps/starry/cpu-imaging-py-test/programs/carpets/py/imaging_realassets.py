#!/usr/bin/env python3
"""imaging_realassets - cross-library decode consistency across the shared media format zoo.

For EACH real image under ASSET_DIR whose format all three libraries support (png / bmp / ppm / pgm /
tiff / jpg / webp), decode it with PIL.Image.open, imageio.v3.imread and skimage.io.imread and assert the
three libraries agree BYTE-FOR-BYTE on shape, dtype and pixel content, and that the SHA-256 of the decoded
RGB buffer is identical across the three. This is the real, format-agnostic cross-library-consistency
property: the three libs decode the SAME container bytes, so even lossy jpg/webp must agree WITH EACH OTHER
(the codec is deterministic; what differs between formats is the stored image, not the decode of a fixed
file). Grayscale (pgm) is broadcast to three channels; RGBA is truncated to RGB - identically for all libs.

In addition, one pinned deterministic red case (programs/sample_red.png, staged next to the cells) is
decoded and asserted red-dominant, exercising a known-content golden independent of the corpus.

A staged asset that a library genuinely cannot decode is a hard failure (see GAP 3 fix below): the corpus
is staged deterministically, so a decode error means a real regression, not an optional codec. The pinned
sample_red.png is committed and always staged, so an empty ASSET_DIR + missing pinned sample is a broken
staging condition and fails the gate rather than passing vacuously.

Determinism: fixed inputs; the corpus is iterated in sorted order; np.random.seed(0x233) is set for any
RNG-touching path (none here, but kept for parity with the other cells).
"""
import hashlib
import os
import glob
import sys

import numpy as np

from img_common import Gate

np.random.seed(0x233)

g = Gate("IMAGING_REALASSETS")

from PIL import Image
import imageio.v3 as iio
from skimage import io as skio

# Formats all three libraries decode. tiff/webp are optional per-lib; a format that a library genuinely
# cannot open is skipped for that file and recorded, not silently dropped.
FORMAT_GLOBS = ("*.png", "*.bmp", "*.ppm", "*.pgm",
                "*.tiff", "*.tif", "*.jpg", "*.jpeg", "*.webp")


def to_rgb(arr):
    """Normalize any decoded array to HxWx3 uint8: grayscale -> broadcast, RGBA -> drop alpha."""
    arr = np.asarray(arr)
    if arr.ndim == 2:
        arr = np.stack([arr] * 3, axis=-1)
    return arr[..., :3]


def decode_all(path):
    """Decode with the three libs; return (pil, iio, sk) RGB arrays or raise for a genuinely absent codec."""
    pil = to_rgb(np.array(Image.open(path).convert("RGB")))
    ii = to_rgb(iio.imread(path))
    sk = to_rgb(skio.imread(path))
    return pil, ii, sk


def assert_consistent(tag, pil, iio_arr, sk_arr):
    g.check(pil.shape == iio_arr.shape == sk_arr.shape,
            "%s: shape mismatch PIL=%s iio=%s sk=%s" % (tag, pil.shape, iio_arr.shape, sk_arr.shape))
    g.check(pil.dtype == iio_arr.dtype == sk_arr.dtype == np.uint8,
            "%s: dtype mismatch or not uint8 (PIL=%s iio=%s sk=%s)" % (tag, pil.dtype, iio_arr.dtype, sk_arr.dtype))
    g.check(np.array_equal(pil, iio_arr), "%s: PIL vs imageio pixels differ" % tag)
    g.check(np.array_equal(pil, sk_arr), "%s: PIL vs skimage pixels differ" % tag)
    g.check(np.array_equal(iio_arr, sk_arr), "%s: imageio vs skimage pixels differ" % tag)
    h_pil = hashlib.sha256(pil.tobytes()).hexdigest()
    h_iio = hashlib.sha256(iio_arr.tobytes()).hexdigest()
    h_sk = hashlib.sha256(sk_arr.tobytes()).hexdigest()
    g.check(h_pil == h_iio == h_sk,
            "%s: decoded-buffer SHA-256 differs (PIL=%s iio=%s sk=%s)" % (tag, h_pil[:12], h_iio[:12], h_sk[:12]))


# ---- corpus leg: iterate every real image of a supported format under ASSET_DIR
asset_dir = os.environ.get(
    "ASSET_DIR",
    os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "assets", "images"),
)
corpus = []
for pat in FORMAT_GLOBS:
    corpus.extend(glob.glob(os.path.join(asset_dir, pat)))
corpus = sorted(set(corpus))

# ---- pinned red case: deterministic known-content golden. On-target prebuild stages it to
# $INSTALL_DIR/sample_red.png (the cell lives at $INSTALL_DIR/py/), i.e. "../sample_red.png"; in the
# source tree the cell is programs/carpets/py/ and the committed file is programs/sample_red.png. Probe
# both so host runs and on-target runs both find it.
_here = os.path.dirname(os.path.abspath(__file__))
red_path = None
for cand in (os.path.join(_here, "..", "sample_red.png"),
             os.path.join(_here, "..", "..", "sample_red.png")):
    if os.path.isfile(cand):
        red_path = cand
        break
if red_path is None:
    red_path = os.path.join(_here, "..", "sample_red.png")  # non-existent default for the skip check

if not corpus and not os.path.isfile(red_path):
    # The pinned sample_red.png is committed and always staged, so an empty state is a provisioning bug,
    # not a legitimate honest-skip - fail the gate rather than pass vacuously.
    g.fail("no real asset image under ASSET_DIR=%s and no pinned sample - staging is broken" % asset_dir)
    raise SystemExit(g.finish())

decoded = 0
for path in corpus:
    name = os.path.basename(path)
    # prebuild stages only formats all three libs are expected to decode, so a decode error on a staged
    # asset is a real regression, not an optional-codec skip - record it as a gate failure, do not drop it.
    try:
        pil, ii, sk = decode_all(path)
    except (OSError, ValueError) as exc:
        g.fail("%s staged but not decodable by all three libs (%s)" % (name, type(exc).__name__))
        continue
    assert_consistent(name, pil, ii, sk)
    decoded += 1

sys.stderr.write("  imaging_realassets: cross-checked %d real corpus images under %s\n" % (decoded, asset_dir))

# ---- pinned red-dominant golden (only this file carries a content assertion)
if os.path.isfile(red_path):
    pil, ii, sk = decode_all(red_path)
    assert_consistent("sample_red.png", pil, ii, sk)
    q = (pil.reshape(-1, 3) // 32) * 32
    vals, counts = np.unique(q, axis=0, return_counts=True)
    dom = tuple(int(v) for v in vals[counts.argmax()])
    g.check(dom[0] > dom[1] and dom[0] > dom[2],
            "pinned sample_red.png expected red-dominant, got %r" % (dom,))

# require at least one real decode when assets are present (guard against a vacuous pass)
g.check(decoded >= 1 or os.path.isfile(red_path),
        "no real image decoded despite ASSET_DIR present")

raise SystemExit(g.finish())
