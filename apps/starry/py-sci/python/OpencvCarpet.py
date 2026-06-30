#!/usr/bin/env python3
# OpencvCarpet.py — exact-assertion carpet for OpenCV (cv2) on musl-native CPython.
#
# Image ops are checked by EXACT pixel values, raw-buffer sha256 of fixed-point integer
# operations, or exact integer arrays — all version-stable (fixed-point BGR2GRAY weights,
# nearest-neighbour replication, morphology min/max, etc. are identical across OpenCV 4.x).
# The cv2 version gate is lenient (major >= 4). Self-contained ok/fail counters; prints
# OPENCV_RESULT then OPENCV_DONE only when fail == 0.
import hashlib
import sys

ok = 0
fail = 0


def chk(name, cond, info=""):
    global ok, fail
    if cond:
        ok += 1
        print("  ok %s%s" % (name, (" " + info) if info else ""))
    else:
        fail += 1
        print("  FAIL %s%s" % (name, (" " + info) if info else ""))


import numpy as np
import cv2

# Lenient version floor (major), never an exact patch string.
chk("version", int(cv2.__version__.split(".")[0]) >= 4, "cv2=%s" % cv2.__version__)

# ---- PROVEN core (4-arch green): EXACT pixels + raw-buffer sha256 ----
# BGR2GRAY uses fixed-point integer weights -> identical across OpenCV versions.
img = np.zeros((4, 4, 3), dtype=np.uint8)
img[:2] = [10, 20, 30]
img[2:] = [200, 100, 50]
gray = cv2.cvtColor(img, cv2.COLOR_BGR2GRAY)
chk("gray_pixels", gray.flatten().tolist() == [22] * 8 + [96] * 8)
chk("gray_sha256",
    hashlib.sha256(gray.tobytes()).hexdigest()
    == "1089201e8d888b0f9ef27f7cac6a0a8278979425c58dcd7da0004e71a0bae46d")
# nearest-neighbour resize = exact pixel replication.
small = np.array([[0, 255], [128, 64]], dtype=np.uint8)
big = cv2.resize(small, (4, 4), interpolation=cv2.INTER_NEAREST)
chk("resize_sha256",
    hashlib.sha256(big.tobytes()).hexdigest()
    == "0c45115614dc9b395f95c1f6f28dbc0aeb8d3fbc16910a49f0fe2c515a59bc1a")
# binary threshold = exact.
_, th = cv2.threshold(np.array([[0, 100, 150, 200]], dtype=np.uint8), 127, 255, cv2.THRESH_BINARY)
chk("threshold", th.flatten().tolist() == [0, 0, 255, 255])
_, thi = cv2.threshold(np.array([[0, 100, 150, 200]], dtype=np.uint8), 127, 255, cv2.THRESH_BINARY_INV)
chk("threshold_inv", thi.flatten().tolist() == [255, 255, 0, 0])
# PNG round-trip: encode then decode reproduces the SAME pixels (lossless), even though the
# encoded byte stream may differ by libpng version.
buf = cv2.imencode(".png", gray)[1]
dec = cv2.imdecode(buf, cv2.IMREAD_GRAYSCALE)
chk("png_roundtrip", np.array_equal(dec, gray))
# BMP round-trip (uncompressed) — second codec path.
buf2 = cv2.imencode(".bmp", img)[1]
dec2 = cv2.imdecode(buf2, cv2.IMREAD_COLOR)
chk("bmp_roundtrip", np.array_equal(dec2, img))

# ---- More cvtColor codes (channel algebra, exact) ----
px = np.array([[[1, 2, 3]]], dtype=np.uint8)  # one BGR pixel
chk("bgr2rgb", cv2.cvtColor(px, cv2.COLOR_BGR2RGB)[0, 0].tolist() == [3, 2, 1])
g1 = np.array([[7]], dtype=np.uint8)
chk("gray2bgr", cv2.cvtColor(g1, cv2.COLOR_GRAY2BGR)[0, 0].tolist() == [7, 7, 7])

# ---- Geometric: warpAffine integer translate + getRotationMatrix2D identity ----
base = np.array([[10, 20, 30], [40, 50, 60], [70, 80, 90]], dtype=np.uint8)
M = np.float32([[1, 0, 1], [0, 1, 0]])  # shift right by 1 column
w = cv2.warpAffine(base, M, (3, 3), flags=cv2.INTER_NEAREST,
                   borderMode=cv2.BORDER_CONSTANT, borderValue=0)
chk("warp_translate", w.tolist() == [[0, 10, 20], [0, 40, 50], [0, 70, 80]])
R = cv2.getRotationMatrix2D((1.0, 1.0), 0.0, 1.0)  # 0 deg, scale 1 -> identity
chk("rotmat_identity", np.allclose(R, np.array([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0]])))
wr = cv2.warpAffine(base, R, (3, 3), flags=cv2.INTER_NEAREST)
chk("warp_identity_noop", np.array_equal(wr, base))

# ---- Borders / flips / transpose (exact integer reshaping) ----
a = np.array([[1, 2], [3, 4]], dtype=np.uint8)
chk("copyMakeBorder",
    cv2.copyMakeBorder(a, 1, 1, 1, 1, cv2.BORDER_CONSTANT, value=0).tolist()
    == [[0, 0, 0, 0], [0, 1, 2, 0], [0, 3, 4, 0], [0, 0, 0, 0]])
chk("flip_vertical", cv2.flip(a, 0).tolist() == [[3, 4], [1, 2]])
chk("flip_horizontal", cv2.flip(a, 1).tolist() == [[2, 1], [4, 3]])
chk("flip_both", cv2.flip(a, -1).tolist() == [[4, 3], [2, 1]])
chk("transpose", cv2.transpose(a).tolist() == [[1, 3], [2, 4]])

# ---- Morphology with a known 3x3 rectangular kernel (min/max, exact) ----
spot = np.zeros((5, 5), dtype=np.uint8)
spot[2, 2] = 255
k3 = cv2.getStructuringElement(cv2.MORPH_RECT, (3, 3))
chk("erode_isolated", int(cv2.erode(spot, k3).sum()) == 0)  # lone pixel eroded away
chk("dilate_isolated",
    cv2.dilate(spot, k3).tolist()
    == [[0, 0, 0, 0, 0], [0, 255, 255, 255, 0], [0, 255, 255, 255, 0],
        [0, 255, 255, 255, 0], [0, 0, 0, 0, 0]])

# ---- filter2D identity kernel == input; integral image; minMaxLoc ----
src = np.array([[1, 2, 3], [4, 5, 6], [7, 8, 9]], dtype=np.uint8)
ident = np.array([[0, 0, 0], [0, 1, 0], [0, 0, 0]], dtype=np.float32)
chk("filter2d_identity", np.array_equal(cv2.filter2D(src, -1, ident), src))
chk("integral",
    cv2.integral(np.ones((2, 2), dtype=np.uint8)).tolist() == [[0, 0, 0], [0, 1, 2], [0, 2, 4]])
mn, mx, mnloc, mxloc = cv2.minMaxLoc(np.array([[1, 5, 3], [9, 2, 7]], dtype=np.uint8))
chk("minmaxloc", mn == 1.0 and mx == 9.0 and mnloc == (0, 0) and mxloc == (0, 1),
    "min=%s max=%s minloc=%s maxloc=%s" % (mn, mx, mnloc, mxloc))

# ---- findContours: exactly one external contour for a single solid square (OpenCV 4.x) ----
canvas = np.zeros((10, 10), dtype=np.uint8)
canvas[3:7, 3:7] = 255
contours, _ = cv2.findContours(canvas, cv2.RETR_EXTERNAL, cv2.CHAIN_APPROX_SIMPLE)
chk("contour_count", len(contours) == 1)
chk("contour_area", int(cv2.contourArea(contours[0])) == 9)  # 3x3 inner polygon area

print("OPENCV_RESULT ok=%d fail=%d" % (ok, fail))
if fail == 0:
    print("OPENCV_DONE")
    sys.exit(0)
sys.exit(1)
