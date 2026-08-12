#!/usr/bin/env python3
"""opencv_draw - 2D drawing primitives vs the analytic shape, per pixel (LINE_8, no anti-aliasing).

rectangle (filled area + clean exterior), line (axis-aligned exact + diagonal), circle (fill samples +
analytic pi r^2 coverage sweep), ellipse, fillPoly/polylines of a known triangle, putText ink-in-bbox.
"""
import math
import cv2
import numpy as np
from cv_common import Gate

cv2.setNumThreads(1)
g = Gate("OPENCV_DRAW")
W, H = 64, 64


def nz(m):
    return int(cv2.countNonZero(m))


# filled rectangle [10..29]x[8..19] -> 20x12 = 240
r = np.zeros((H, W), dtype=np.uint8)
cv2.rectangle(r, (10, 8), (29, 19), 255, cv2.FILLED, cv2.LINE_8)
g.check(nz(r) == 240, "filled rectangle area != 240")
mask = np.zeros((H, W), dtype=np.uint8)
mask[8:20, 10:30] = 255
g.check(np.array_equal(r, mask), "filled rectangle != exact analytic mask")

# horizontal line row 30, x 5..25
ln = np.zeros((H, W), dtype=np.uint8)
cv2.line(ln, (5, 30), (25, 30), 255, 1, cv2.LINE_8)
g.check(nz(ln) == 21 and np.all(ln[30, 5:26] == 255), "horizontal line != 21 exact pixels")
g.check(ln[29, 15] == 0 and ln[31, 15] == 0, "line leaked to adjacent rows")
lv = np.zeros((H, W), dtype=np.uint8)
cv2.line(lv, (40, 5), (40, 35), 255, 1, cv2.LINE_8)
g.check(nz(lv) == 31, "vertical line != 31 pixels")
ld = np.zeros((H, W), dtype=np.uint8)
cv2.line(ld, (0, 0), (20, 20), 255, 1, cv2.LINE_8)
g.check(all(ld[i, i] == 255 for i in range(21)), "diagonal misses (i,i)")

# filled circle center (32,32) r=12
cc = np.zeros((H, W), dtype=np.uint8)
cv2.circle(cc, (32, 32), 12, 255, cv2.FILLED, cv2.LINE_8)
g.check(cc[32, 32] == 255, "circle center not filled")
g.check(cc[32, 42] == 255 and cc[22, 32] == 255, "circle r=10 samples not filled")
g.check(cc[32, 46] == 0 and cc[46, 32] == 0, "circle r=14 samples not clean")
area, ideal = nz(cc), math.pi * 144
g.check(abs(area - ideal) / ideal < 0.08, "circle area not within 8% pi r^2")
yy, xx = np.mgrid[0:H, 0:W]
dist = np.hypot(xx - 32, yy - 32)
g.check(np.all(cc[dist <= 12 - 1.5] == 255) and np.all(cc[dist >= 12 + 1.5] == 0),
        "circle analytic coverage sweep failed")

# ellipse center (32,32) axes (16,8)
el = np.zeros((H, W), dtype=np.uint8)
cv2.ellipse(el, (32, 32), (16, 8), 0, 0, 360, 255, cv2.FILLED, cv2.LINE_8)
g.check(el[32, 32] == 255 and el[0, 0] == 0, "ellipse center/corner wrong")
g.check(el[32, 47] == 255 and el[39, 32] == 255, "ellipse inside-axis not filled")
g.check(el[32, 50] == 0 and el[42, 32] == 0, "ellipse outside-axis not clean")

# fillPoly / polylines of a right triangle (10,10),(30,10),(10,30)
tri = np.array([[10, 10], [30, 10], [10, 30]], dtype=np.int32)
tr = np.zeros((H, W), dtype=np.uint8)
cv2.fillPoly(tr, [tri], 255, cv2.LINE_8)
g.check(tr[12, 12] == 255, "fillPoly interior not set")
g.check(tr[25, 25] == 0, "fillPoly leaked past hypotenuse")
g.check(tr[10, 10] == 255 and tr[10, 29] == 255, "fillPoly corners not set")
pl = np.zeros((H, W), dtype=np.uint8)
cv2.polylines(pl, [tri], True, 255, 1, cv2.LINE_8)
g.check(int(np.count_nonzero(pl[10, 10:31] == 255)) == 21, "polylines top edge != 21")

# putText
tx = np.zeros((32, 96), dtype=np.uint8)
cv2.putText(tx, "Hi", (4, 22), cv2.FONT_HERSHEY_SIMPLEX, 0.8, 255, 1, cv2.LINE_8)
g.check(nz(tx) > 0, "putText produced no ink")
g.check(tx[31, 95] == 0 and tx[0, 95] == 0, "putText leaked to far corners")
g.check(nz(tx[:, :48]) == nz(tx), "putText ink not confined to left half")

raise SystemExit(g.finish())
