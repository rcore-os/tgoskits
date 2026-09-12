#!/usr/bin/env python3
"""opencv_color - cvtColor conversions vs the closed-form color matrix, per pixel.

BGR<->RGB (byte-exact channel swap), BGR->GRAY (BT.601 fixed-point closed form), BGR<->HSV (known primaries),
BGR<->YCrCb, BGR<->I420 (4:2:0, luma plane == studio-swing BT.601). Known image; every pixel asserted.
"""
import cv2
import numpy as np
from cv_common import Gate, bgr2gray_601

cv2.setNumThreads(1)
g = Gate("OPENCV_COLOR")

# 2x2 BGR image (memory order is BGR): red / green / blue / gray
bgr = np.array([[[0, 0, 255], [0, 255, 0]],
                [[255, 0, 0], [128, 128, 128]]], dtype=np.uint8)

# BGR->RGB exact channel swap
rgb = cv2.cvtColor(bgr, cv2.COLOR_BGR2RGB)
g.check(np.array_equal(rgb, bgr[:, :, ::-1]), "BGR2RGB not an exact B<->R swap")

# round-trip identity
back = cv2.cvtColor(rgb, cv2.COLOR_RGB2BGR)
g.check(np.array_equal(back, bgr), "BGR->RGB->BGR not identity")

# BGR->GRAY == BT.601 fixed-point closed form, per pixel
gray = cv2.cvtColor(bgr, cv2.COLOR_BGR2GRAY)
want = np.array([[bgr2gray_601(*(int(v) for v in bgr[y, x])) for x in range(2)]
                 for y in range(2)], dtype=np.uint8)
g.check(np.array_equal(gray, want), "BGR2GRAY != BT.601 closed form")
g.check(gray[0, 0] == 76 and gray[0, 1] == 150 and gray[1, 0] == 29 and gray[1, 1] == 128,
        "gray pins (76,150,29,128) wrong")

# BGR->YCrCb: gray -> (128,128,128); Y channel == gray closed form
ycc = cv2.cvtColor(bgr, cv2.COLOR_BGR2YCrCb)
g.check(tuple(ycc[1, 1]) == (128, 128, 128), "YCrCb(gray) != (128,128,128)")
g.check(np.all(np.abs(ycc[:, :, 0].astype(int) - gray.astype(int)) <= 1), "YCrCb luma != gray")

# BGR->HSV: known primaries
hsv = cv2.cvtColor(bgr, cv2.COLOR_BGR2HSV)
g.check(tuple(hsv[0, 0]) == (0, 255, 255), "HSV(red) != (0,255,255)")
g.check(tuple(hsv[0, 1]) == (60, 255, 255), "HSV(green) != (60,255,255)")
g.check(tuple(hsv[1, 0]) == (120, 255, 255), "HSV(blue) != (120,255,255)")
g.check(hsv[1, 1][1] == 0 and hsv[1, 1][2] == 128, "HSV(gray) S/V != (0,128)")

# HSV round-trip
hsv2bgr = cv2.cvtColor(hsv, cv2.COLOR_HSV2BGR)
g.check(np.all(np.abs(hsv2bgr.astype(int) - bgr.astype(int)) <= 2), "HSV->BGR drifted > 2")

# BGR->I420: (H*3/2)xW; Y plane == studio-swing BT.601 closed form
big = np.zeros((4, 4, 3), dtype=np.uint8)
for y in range(4):
    for x in range(4):
        big[y, x] = [(x * 40) & 0xff, (y * 40) & 0xff, ((x + y) * 30) & 0xff]
i420 = cv2.cvtColor(big, cv2.COLOR_BGR2YUV_I420)
g.check(i420.shape == (6, 4), "I420 shape != (6,4)")
ywant = np.zeros((4, 4), dtype=int)
for y in range(4):
    for x in range(4):
        b, gg, r = (int(v) for v in big[y, x])
        ywant[y, x] = ((r * 66 + gg * 129 + b * 25 + 128) >> 8) + 16
g.check(np.all(np.abs(i420[:4, :4].astype(int) - ywant) <= 1), "I420 Y != BT.601 studio-swing")

# I420 -> BGR round-trip shape
i420back = cv2.cvtColor(i420, cv2.COLOR_YUV2BGR_I420)
g.check(i420back.shape == (4, 4, 3), "I420->BGR shape wrong")

raise SystemExit(g.finish())
