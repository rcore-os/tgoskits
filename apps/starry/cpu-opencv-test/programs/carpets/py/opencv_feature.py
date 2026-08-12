#!/usr/bin/env python3
"""opencv_feature - feature/edge detectors on KNOWN geometry vs the known answer.

Canny on a vertical step edge -> edges at the known column; cornerHarris / goodFeaturesToTrack on a
checkerboard -> corners at the known grid intersections; HoughLinesP on a drawn line -> the known params.
"""
import math
import cv2
import numpy as np
from cv_common import Gate

cv2.setNumThreads(1)
g = Gate("OPENCV_FEATURE")

# Canny on a vertical step edge at column 20
step = np.zeros((40, 40), dtype=np.uint8)
step[:, 20:] = 255
edges = cv2.Canny(step, 50, 150)
edge_cols_ok, edge_rows = True, 0
for y in range(1, 39):
    xs = np.nonzero(edges[y])[0]
    if len(xs):
        edge_rows += 1
        if xs.min() < 18 or xs.max() > 21:
            edge_cols_ok = False
g.check(edge_cols_ok, "Canny edge not localized at step column (18..21)")
g.check(edge_rows >= 36, "Canny missed the edge on many interior rows")
g.check(edges[20, 5] == 0 and edges[20, 35] == 0, "Canny fired in a flat region")

# cornerHarris on a checkerboard
board = np.zeros((40, 40), dtype=np.uint8)
for by in range(4):
    for bx in range(4):
        if (bx + by) & 1:
            board[by * 10:by * 10 + 10, bx * 10:bx * 10 + 10] = 255
harris = cv2.cornerHarris(board, 2, 3, 0.04)
hmax = float(harris.max())
g.check(hmax > 0, "cornerHarris produced no positive response")
g.check(harris[20, 20] > 0.2 * hmax, "Harris weak at intersection (20,20)")
g.check(abs(harris[5, 5]) < 0.05 * hmax, "Harris not ~0 in flat square (5,5)")

# goodFeaturesToTrack snaps to grid
corners = cv2.goodFeaturesToTrack(board, 20, 0.1, 5)
g.check(corners is not None and len(corners) > 0, "goodFeaturesToTrack found nothing")
on_grid = True
for c in corners.reshape(-1, 2):
    if abs(c[0] - round(c[0] / 10) * 10) > 2.5 or abs(c[1] - round(c[1] / 10) * 10) > 2.5:
        on_grid = False
g.check(on_grid, "a detected corner is not near a grid intersection")

# HoughLinesP on a horizontal line at row 25
lineimg = np.zeros((60, 60), dtype=np.uint8)
cv2.line(lineimg, (5, 25), (54, 25), 255, 1, cv2.LINE_8)
lines = cv2.HoughLinesP(lineimg, 1, math.pi / 180.0, 30, minLineLength=30, maxLineGap=5)
g.check(lines is not None and len(lines) > 0, "HoughLinesP found no line")
found_h = False
for x1, y1, x2, y2 in lines.reshape(-1, 4):
    if abs(int(y1) - int(y2)) <= 1 and abs(int(y1) - 25) <= 1 and abs(int(x2) - int(x1)) >= 30:
        found_h = True
g.check(found_h, "HoughLinesP did not recover the horizontal line at row 25")

raise SystemExit(g.finish())
