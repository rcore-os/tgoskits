#!/usr/bin/env python3
"""gen_goldens.py - re-derive every pinned constant in the cpu-imaging-py-test carpet from first principles.

Run on the host (any Python with numpy + Pillow + scikit-image) to review the goldens the cells assert.
This prints the derivations; it does not run the carpet.
"""
import numpy as np


def luma_601_round(r, g, b):
    return (r * 19595 + g * 38470 + b * 7471 + 0x8000) >> 16


print("== PIL RGB->L (ITU-R 601-2, Pillow L24 fixed point: (R*19595+G*38470+B*7471+0x8000)>>16) ==")
for name, (r, g, b) in [("red", (255, 0, 0)), ("green", (0, 255, 0)), ("blue", (0, 0, 255)),
                        ("(123,231,45)", (123, 231, 45))]:
    print("  L(%-12s) = %3d   (real 601-2 = %.3f)"
          % (name, luma_601_round(r, g, b), r * .299 + g * .587 + b * .114))

print("\n== PIL BILINEAR 2x upscale of [0,90,180,255] (src center map: c=(dx+0.5)*0.5-0.5, clamp, round) ==")
row = [0, 90, 180, 255]
out = []
for dx in range(8):
    c = max(0.0, min(3.0, (dx + 0.5) * 0.5 - 0.5))
    x0 = int(np.floor(c)); x1 = min(x0 + 1, 3); f = c - x0
    out.append(int(row[x0] * (1 - f) + row[x1] * f + 0.5))
print("  ", out)

print("\n== skimage rgb2gray (BT.709 luma): 0.2125 R + 0.7154 G + 0.0721 B ==")
print("  pure green ->", 0.7154, " pure red ->", 0.2125, " pure blue ->", 0.0721)

print("\n== skimage sobel of a unit-slope ramp ==")
ramp = np.tile(np.arange(10, dtype=float), (10, 1))
from skimage import filters
print("  sobel   interior =", round(float(filters.sobel(ramp)[4, 4]), 6), "(== sqrt(2) =", round(2 ** .5, 6), ")")
print("  sobel_v interior =", round(float(filters.sobel_v(ramp)[4, 4]), 6))

print("\n== skimage regionprops of two blobs ==")
from skimage import measure
blobs = np.zeros((10, 10), bool); blobs[1:4, 1:4] = True; blobs[6:8, 6:9] = True
lbl = measure.label(blobs)
for p in measure.regionprops(lbl):
    print("  area=%d centroid=%s bbox=%s" % (p.area, tuple(np.round(p.centroid, 3)), p.bbox))

print("\n== JPEG PSNR floors: q95 on a smooth 32x32 gradient (PIL >30 dB, imageio default >28 dB) ==")
print("  random noise is worst-case for JPEG and is intentionally NOT used for the PSNR legs")
