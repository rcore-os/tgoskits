#!/usr/bin/env python3
"""gen_goldens.py - re-derive the closed-form constants the cpu-opencv-test cells assert against, for review.

These are the numpy / first-principles goldens (NOT OpenCV outputs) that the cells pin. Run it to sanity-check
the magic numbers embedded in the C++/Python cells. Requires only numpy (cv2 optional, used to cross-check the
Gaussian taps and the BT.601 luma if available).
"""
import numpy as np


def sep(t):
    print("\n== %s ==" % t)


# opencv_mat: matmul A(2x3) * A^T
sep("mat")
A = np.array([[1., 2., 3.], [4., 5., 6.]])
print("A @ A^T =", (A @ A.T).tolist(), " (cells pin [[14,32],[32,77]])")
print("det([[1,2],[3,4]]) =", np.linalg.det(np.array([[1., 2.], [3., 4.]])), " (cells pin -2)")

# opencv_color: BT.601 luma (fixed-point) for the four primaries
sep("color")


def gray601(b, g, r):
    return (r * 4899 + g * 9617 + b * 1868 + 8192) >> 14


for name, (b, g, r) in [("red", (0, 0, 255)), ("green", (0, 255, 0)),
                        ("blue", (255, 0, 0)), ("gray", (128, 128, 128))]:
    print("gray601(%s) = %d" % (name, gray601(b, g, r)), " (cells pin 76/150/29/128)")
print("I420 studio-swing luma = ((R*66+G*129+B*25+128)>>8)+16")

# opencv_filter: getGaussianKernel(5,1.0) taps (OpenCV formula reproduced without cv2)
sep("filter")


def gaussian_kernel(ks, sigma):
    # OpenCV: if sigma<=0 it derives sigma=0.3*((ks-1)*0.5-1)+0.8; here sigma is given (=1.0).
    c = (ks - 1) * 0.5
    w = np.array([np.exp(-((i - c) ** 2) / (2 * sigma * sigma)) for i in range(ks)])
    return w / w.sum()


k = gaussian_kernel(5, 1.0)
print("getGaussianKernel(5,1.0) ~", [round(v, 6) for v in k], " (cells pin 0.054489/0.244201/0.40262)")
print("center^2 =", round(k[2] ** 2, 6))
print("Sobel-x of a 10*x ramp -> 8*slope = 80 interior")

# opencv_geometry: bilinear dst(1,1) of [[0,10],[20,30]] scaled 2x2->4x4
sep("geometry")
src = np.array([[0., 10.], [20., 30.]])
sx = 2.0 / 4.0
fx = (1 + 0.5) * sx - 0.5
fy = (1 + 0.5) * sx - 0.5  # =0.25
x0, y0 = int(np.floor(fx)), int(np.floor(fy))
ax, ay = fx - x0, fy - y0
val = ((1 - ay) * ((1 - ax) * src[y0, x0] + ax * src[y0, x0 + 1]) +
       ay * ((1 - ax) * src[y0 + 1, x0] + ax * src[y0 + 1, x0 + 1]))
print("bilinear dst(1,1) =", val, "-> round", round(val), " (cells pin ~8)")
print("getRotationMatrix2D(c,90,1): cos=0 sin=1 -> [[0,1,..],[-1,0,..]]")

# opencv_morph: dilate(dot) area, connectedComponents count
sep("morph")
print("dilate(single dot, 3x3 rect SE) area = 9 (3x3 block)")
print("connectedComponents(3 separated blobs, 8-conn) = 4 (bg + 3)")

# opencv_draw: rectangle area, circle area
sep("draw")
print("rectangle pt1(10,8) pt2(29,19) FILLED area = 20*12 =", 20 * 12)
print("filled circle r=12 area ~ pi r^2 =", round(np.pi * 144, 1), "(assert within 8%)")

# opencv_io: PSNR floor rationale
sep("io")
print("lossless PNG/BMP/PPM/TIFF/WebP -> byte-exact; JPEG q95 -> PSNR > 35 dB on a gradient (lossy)")
print("FFV1 clip: 5 frames, first-frame marker pixel BGR (0,1,2), channel-0 mean 0")
