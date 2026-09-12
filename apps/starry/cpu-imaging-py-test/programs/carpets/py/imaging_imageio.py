#!/usr/bin/env python3
"""imaging_imageio - imageio v3 per-API assertions vs numpy goldens.

Drives real imageio.v3 imread/imwrite/imencode/imdecode on KNOWN arrays and asserts:
lossless formats (PNG/BMP/TIFF/PPM/PGM) round-trip BYTE-EXACT vs the source np.array; a lossy JPEG clears
a PSNR floor; a synthetic multi-frame stack (GIF + volumetric TIFF) round-trips frame-exact with the right
shape/dtype; and in-memory encode/decode ("<bytes>") equals the source. The mp4/ffmpeg video leg is
excluded on purpose: its imageio_ffmpeg binary is not available for all four target arches, so it cannot
really run everywhere and is dropped rather than counted as a skip-as-pass.
"""
import numpy as np
import imageio.v3 as iio

from img_common import Gate

g = Gate("IMAGING_IMAGEIO")

np.random.seed(0x233)
rgb = (np.random.rand(8, 8, 3) * 255).astype(np.uint8)
gray = (np.random.rand(8, 8) * 255).astype(np.uint8)


def roundtrip(arr, ext):
    b = iio.imwrite("<bytes>", arr, extension=ext)
    return iio.imread(b, extension=ext), b


# ---------------------------------------------------------------- lossless byte-exact round-trips
for ext in (".png", ".bmp", ".tiff", ".ppm"):
    r, _ = roundtrip(rgb, ext)
    g.check(np.array_equal(r, rgb), "%s RGB round-trip not byte-exact" % ext)
    g.check(r.shape == (8, 8, 3) and r.dtype == np.uint8, "%s shape/dtype wrong" % ext)

# grayscale PNG / PGM
for ext in (".png", ".pgm"):
    r, _ = roundtrip(gray, ext)
    g.check(np.array_equal(r, gray), "%s L round-trip not byte-exact" % ext)

# ---------------------------------------------------------------- in-memory encode/decode == source
encoded = iio.imwrite("<bytes>", rgb, extension=".png")
g.check(isinstance(encoded, (bytes, bytearray)) and len(encoded) > 0, "imwrite<bytes> produced no bytes")
decoded = iio.imread(encoded, extension=".png")
g.check(np.array_equal(decoded, rgb), "imdecode(imencode) != source")

# ---------------------------------------------------------------- lossy JPEG PSNR floor (smooth image)
yy, xx = np.mgrid[0:32, 0:32]
smooth = np.stack([(xx * 8) % 256, (yy * 8) % 256, ((xx + yy) * 4) % 256], -1).astype(np.uint8)
jb = iio.imwrite("<bytes>", smooth, extension=".jpg")  # default quality
jr = iio.imread(jb, extension=".jpg").astype(np.float64)
mse = np.mean((jr - smooth.astype(np.float64)) ** 2)
psnr = 10 * np.log10(255.0 ** 2 / mse) if mse > 0 else 99.0
g.check(psnr > 28.0, "JPEG PSNR %.1f below floor" % psnr)
g.check(jr.shape == (32, 32, 3), "JPEG decoded shape wrong")

# ---------------------------------------------------------------- multi-frame stack (mimwrite/mimread)
# a synthetic 4-frame stack whose frame i is a solid ramp; GIF is palette-lossless for a single grey ramp,
# and a volumetric TIFF is byte-exact.
frames = np.stack([np.full((6, 6), i * 40, np.uint8) for i in range(4)])  # (4,6,6)
gb = iio.imwrite("<bytes>", frames, extension=".gif")
gr = iio.imread(gb, extension=".gif", index=None)  # all frames
g.check(gr.shape[0] == 4, "GIF frame count != 4")
# GIF frames come back RGB (palette expanded); each frame is a constant field of the source value
for i in range(4):
    fr = gr[i]
    val = fr[..., 0] if fr.ndim == 3 else fr
    g.check(np.all(val == i * 40), "GIF frame %d not a constant %d field" % (i, i * 40))

# volumetric TIFF: byte-exact frame stack (imageio volread analog)
vol = (np.arange(3 * 5 * 5).reshape(3, 5, 5) % 256).astype(np.uint8)
vb = iio.imwrite("<bytes>", vol, extension=".tiff")
vr = iio.imread(vb, extension=".tiff", index=None)
g.check(vr.shape == (3, 5, 5) and np.array_equal(vr, vol), "volumetric TIFF stack not frame-exact")

# ---------------------------------------------------------------- metadata / properties
props = iio.improps(encoded, extension=".png")
g.check(tuple(props.shape) == (8, 8, 3) and props.dtype == np.uint8, "improps shape/dtype wrong")

# The mp4/ffmpeg video leg is intentionally excluded: it requires the imageio_ffmpeg plugin, whose bundled
# ffmpeg binary ships only for a subset of platforms (no riscv64/loongarch64), so an on-target mp4 assertion
# cannot really run on all four arches. Counting a never-executed leg as a pass would be a skip-as-pass hole,
# so the leg is dropped rather than skipped. The lossless/lossy still-image codecs above are the real bite.

raise SystemExit(g.finish())
