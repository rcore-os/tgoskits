#!/usr/bin/env python3
"""opencv_morph - thresholding & morphology vs closed form.

threshold BINARY/INV/TRUNC on a known ramp (exact split), Otsu on a bimodal histogram, erode/dilate/open/
close of a known binary pattern vs the structuring-element result, connectedComponents count. No RNG.
"""
import cv2
import numpy as np
from cv_common import Gate

cv2.setNumThreads(1)
g = Gate("OPENCV_MORPH")

ramp = np.array([[0, 50, 100, 150, 200]], dtype=np.uint8)
_, th = cv2.threshold(ramp, 100, 255, cv2.THRESH_BINARY)
g.check(list(th[0]) == [0, 0, 0, 255, 255], "THRESH_BINARY split wrong")
_, thi = cv2.threshold(ramp, 100, 255, cv2.THRESH_BINARY_INV)
g.check(thi[0, 2] == 255 and thi[0, 3] == 0, "THRESH_BINARY_INV split wrong")
_, tt = cv2.threshold(ramp, 100, 0, cv2.THRESH_TRUNC)
g.check(tt[0, 3] == 100 and tt[0, 1] == 50, "THRESH_TRUNC wrong")

# Otsu bimodal
bim = np.array([[20, 20, 20, 20, 200, 200, 200, 200]], dtype=np.uint8)
otsu_t, ot = cv2.threshold(bim, 0, 255, cv2.THRESH_BINARY | cv2.THRESH_OTSU)
g.check(20 <= otsu_t < 200, "Otsu threshold not in mode-separating range")
g.check(list(ot[0]) == [0, 0, 0, 0, 255, 255, 255, 255], "Otsu binarization did not separate modes")

# dilate/erode a single dot with 3x3 rect SE
dot = np.zeros((7, 7), dtype=np.uint8)
dot[3, 3] = 255
se = cv2.getStructuringElement(cv2.MORPH_RECT, (3, 3))
dil = cv2.dilate(dot, se)
g.check(cv2.countNonZero(dil) == 9, "dilate(dot) != 9-px block")
g.check(np.all(dil[2:5, 2:5] == 255), "dilate block not centered")
ero = cv2.erode(dil, se)
g.check(cv2.countNonZero(ero) == 1 and ero[3, 3] == 255, "erode(dilate(dot)) != dot")

# opening removes a lone speck
speck = np.zeros((7, 7), dtype=np.uint8)
speck[1, 1] = 255
opened = cv2.morphologyEx(speck, cv2.MORPH_OPEN, se)
g.check(cv2.countNonZero(opened) == 0, "opening did not remove speck")

# closing fills a lone hole
solid = np.full((7, 7), 255, dtype=np.uint8)
solid[3, 3] = 0
closed = cv2.morphologyEx(solid, cv2.MORPH_CLOSE, se)
g.check(closed[3, 3] == 255, "closing did not fill hole")

# connectedComponents: 3 blobs -> 4 labels incl background
blobs = np.zeros((9, 9), dtype=np.uint8)
blobs[1, 1] = blobs[1, 7] = blobs[7, 4] = 255
n, labels = cv2.connectedComponents(blobs, connectivity=8)
g.check(n == 4, "connectedComponents count != 4")
l1, l2, l3 = labels[1, 1], labels[1, 7], labels[7, 4]
g.check(l1 and l2 and l3 and len({l1, l2, l3}) == 3 and labels[0, 0] == 0,
        "blob labels not distinct / background not 0")

raise SystemExit(g.finish())
