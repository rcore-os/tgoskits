#!/usr/bin/env python3
"""opencv_geometry - geometric transforms vs closed form, at known points.

resize NEAREST (block replication) / LINEAR (bilinear closed form), flip, transpose, warpAffine translation
(exact pixel shift), getRotationMatrix2D matrix values, 90-degree rotation exact mapping, getAffineTransform.
"""
import cv2
import numpy as np
from cv_common import Gate

cv2.setNumThreads(1)
g = Gate("OPENCV_GEOMETRY")

src = np.array([[10, 20], [30, 40]], dtype=np.uint8)

# resize x2 NEAREST: block replication
up = cv2.resize(src, (4, 4), interpolation=cv2.INTER_NEAREST)
g.check(all(up[y, x] == src[y // 2, x // 2] for y in range(4) for x in range(4)),
        "resize NEAREST != block replication")

# resize LINEAR bilinear: dst(1,1) of [[0,10],[20,30]]->4x4 == 7.5 -> 8
lin = np.array([[0, 10], [20, 30]], dtype=np.uint8)
linup = cv2.resize(lin, (4, 4), interpolation=cv2.INTER_LINEAR)
g.check(abs(int(linup[1, 1]) - 8) <= 1, "bilinear dst(1,1) != ~7.5")
g.check(linup[0, 0] == 0, "bilinear corner(0,0) != 0")

# flips
fh = cv2.flip(src, 1)
g.check(fh[0, 0] == 20 and fh[0, 1] == 10 and fh[1, 0] == 40 and fh[1, 1] == 30, "flip H mismatch")
fv = cv2.flip(src, 0)
g.check(fv[0, 0] == 30 and fv[1, 0] == 10, "flip V mismatch")

# transpose
tp = cv2.transpose(src)
g.check(tp[0, 1] == src[1, 0] and tp[1, 0] == src[0, 1], "transpose mismatch")

# warpAffine translation +1/+1
marker = np.zeros((5, 5), dtype=np.uint8)
marker[1, 1] = 200
Tm = np.array([[1, 0, 1], [0, 1, 1]], dtype=np.float64)
shifted = cv2.warpAffine(marker, Tm, (5, 5), flags=cv2.INTER_NEAREST)
g.check(shifted[2, 2] == 200, "translation did not move marker to (2,2)")
g.check(shifted[1, 1] == 0, "translation left a ghost")

# getRotationMatrix2D(center,90,1)
R = cv2.getRotationMatrix2D((2, 2), 90.0, 1.0)
g.check(abs(R[0, 0]) < 1e-9 and abs(R[0, 1] - 1) < 1e-9 and abs(R[1, 0] + 1) < 1e-9 and abs(R[1, 1]) < 1e-9,
        "getRotationMatrix2D(90) cos/sin block wrong")
mx = R[0, 0] * 2 + R[0, 1] * 2 + R[0, 2]
my = R[1, 0] * 2 + R[1, 1] * 2 + R[1, 2]
g.check(abs(mx - 2) < 1e-9 and abs(my - 2) < 1e-9, "rotation does not fix its center")

# warpAffine 90deg maps marker to closed-form location
rimg = np.zeros((5, 5), dtype=np.uint8)
rimg[2, 1] = 150  # (x=1,y=2)
rot = cv2.warpAffine(rimg, R, (5, 5), flags=cv2.INTER_NEAREST)
dx = int(round(R[0, 0] * 1 + R[0, 1] * 2 + R[0, 2]))
dy = int(round(R[1, 0] * 1 + R[1, 1] * 2 + R[1, 2]))
g.check(0 <= dx < 5 and 0 <= dy < 5 and rot[dy, dx] == 150, "90deg marker not at closed-form (dx,dy)")

# getAffineTransform of a +1/+1 shift
s3 = np.float32([[0, 0], [1, 0], [0, 1]])
d3 = np.float32([[1, 1], [2, 1], [1, 2]])
AT = cv2.getAffineTransform(s3, d3)
g.check(abs(AT[0, 0] - 1) < 1e-6 and abs(AT[0, 2] - 1) < 1e-6 and abs(AT[1, 1] - 1) < 1e-6 and
        abs(AT[1, 2] - 1) < 1e-6, "getAffineTransform of +1/+1 shift wrong")

raise SystemExit(g.finish())
