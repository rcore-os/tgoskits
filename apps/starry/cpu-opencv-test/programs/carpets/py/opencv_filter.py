#!/usr/bin/env python3
"""opencv_filter - convolution/filtering vs closed form, per pixel.

GaussianBlur(impulse) == normalized separable Gaussian kernel (outer product of getGaussianKernel, exact
taps pinned); Sobel(ramp) == constant derivative; boxFilter/blur(constant) == constant; medianBlur removes
a lone outlier; filter2D(identity) == input. No RNG.
"""
import cv2
import numpy as np
from cv_common import Gate

cv2.setNumThreads(1)
g = Gate("OPENCV_FILTER")

N, c = 9, 4
ks, sigma = 5, 1.0

# GaussianBlur impulse -> outer(k,k)
impulse = np.zeros((N, N), dtype=np.float32)
impulse[c, c] = 1.0
blur = cv2.GaussianBlur(impulse, (ks, ks), sigma, sigmaY=sigma, borderType=cv2.BORDER_CONSTANT)
k1 = cv2.getGaussianKernel(ks, sigma).astype(np.float32).flatten()
kern2d = np.outer(k1, k1)
window = blur[c - 2:c + 3, c - 2:c + 3]
g.check(np.allclose(window, kern2d, atol=1e-6), "GaussianBlur(impulse) != outer(k,k)")
g.check(abs(window.sum() - 1.0) < 1e-5, "Gaussian kernel does not sum to 1")
# exact getGaussianKernel(5,1.0) taps
g.check(abs(k1[0] - 0.054489) < 1e-4 and abs(k1[1] - 0.244201) < 1e-4 and abs(k1[2] - 0.40262) < 1e-4,
        "Gaussian taps != [0.054489,0.244201,0.40262,...]")
g.check(abs(blur[c, c] - 0.40262 ** 2) < 1e-4, "center pixel != w0^2")
g.check(blur[0, 0] == 0.0 and blur[N - 1, N - 1] == 0.0, "Gaussian leaked outside support")

# Sobel of a linear ramp f=10x -> d/dx scaled by 8 => 80 interior; d/dy => 0
ramp = np.zeros((7, 7), dtype=np.float32)
for x in range(7):
    ramp[:, x] = 10.0 * x
sx = cv2.Sobel(ramp, cv2.CV_32F, 1, 0, ksize=3, borderType=cv2.BORDER_REPLICATE)
g.check(np.allclose(sx[1:6, 1:6], 80.0, atol=1e-4), "Sobel-x(ramp) != 80 interior")
sy = cv2.Sobel(ramp, cv2.CV_32F, 0, 1, ksize=3, borderType=cv2.BORDER_REPLICATE)
g.check(np.allclose(sy[1:6, 1:6], 0.0, atol=1e-4), "Sobel-y(x-ramp) != 0")

# boxFilter / blur of a constant
konst = np.full((8, 8), 42.0, dtype=np.float32)
box = cv2.boxFilter(konst, cv2.CV_32F, (3, 3), normalize=True, borderType=cv2.BORDER_REPLICATE)
g.check(np.allclose(box, 42.0, atol=1e-4), "boxFilter(const) != const")
blr = cv2.blur(konst, (5, 5), borderType=cv2.BORDER_REPLICATE)
g.check(np.allclose(blr, 42.0, atol=1e-4), "blur(const) != const")

# medianBlur removes a lone outlier
med_in = np.full((7, 7), 100, dtype=np.uint8)
med_in[3, 3] = 255
med = cv2.medianBlur(med_in, 3)
g.check(med[3, 3] == 100, "medianBlur did not remove outlier")
g.check(np.all(med[1:6, 1:6] == 100), "medianBlur disturbed constant field")

# filter2D identity
idk = np.zeros((3, 3), dtype=np.float32)
idk[1, 1] = 1.0
idout = cv2.filter2D(ramp, cv2.CV_32F, idk)
g.check(np.allclose(idout[1:6, 1:6], ramp[1:6, 1:6], atol=1e-4), "filter2D(identity) != input")

raise SystemExit(g.finish())
