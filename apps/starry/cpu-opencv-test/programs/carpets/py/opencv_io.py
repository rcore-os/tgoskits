#!/usr/bin/env python3
"""opencv_io - codec/container round-trips vs byte-exact / PSNR goldens.

imencode/imdecode PNG/BMP/PPM/TIFF/WebP (lossless -> byte-exact), JPEG (lossy -> PSNR), PGM gray,
imwrite/imread file round-trip, VideoWriter+VideoCapture synthetic clip (FFV1 lossless -> exact frame count
+ first-frame content). Real-asset leg reads ASSET_DIR (honest-skip if none). Seeded RNG 0x233.
"""
import os
import math
import cv2
import numpy as np
from cv_common import Gate

cv2.setNumThreads(1)
g = Gate("OPENCV_IO")


def psnr(a, b):
    mse = np.mean((a.astype(np.float64) - b.astype(np.float64)) ** 2)
    return 1e9 if mse <= 1e-10 else 10 * math.log10(255 * 255 / mse)


rng = np.random.RandomState(0x233)
img = rng.randint(0, 256, (24, 32, 3), dtype=np.uint8)

for ext in ['.png', '.bmp', '.ppm', '.tiff', '.webp']:
    ok, buf = cv2.imencode(ext, img)
    dec = cv2.imdecode(buf, cv2.IMREAD_COLOR) if ok else None
    g.check(ok and dec is not None and np.array_equal(dec, img),
            "lossless round-trip not byte-exact: %s" % ext)

# JPEG lossy on a gradient
grad = np.zeros((24, 32, 3), dtype=np.uint8)
for y in range(24):
    for x in range(32):
        grad[y, x] = [(x * 8) & 255, (y * 10) & 255, ((x + y) * 4) & 255]
ok, buf = cv2.imencode('.jpg', grad, [cv2.IMWRITE_JPEG_QUALITY, 95])
dec = cv2.imdecode(buf, cv2.IMREAD_COLOR) if ok else None
g.check(ok and dec is not None and dec.shape == grad.shape, "JPEG encode/decode failed")
g.check(dec is not None and psnr(grad, dec) > 35.0, "JPEG q95 PSNR below 35 dB")
g.check(dec is not None and not np.array_equal(dec, grad), "JPEG unexpectedly byte-exact")

# PGM gray
gray = cv2.cvtColor(img, cv2.COLOR_BGR2GRAY)
ok, buf = cv2.imencode('.pgm', gray)
dec = cv2.imdecode(buf, cv2.IMREAD_GRAYSCALE) if ok else None
g.check(ok and dec is not None and np.array_equal(dec, gray), "PGM gray round-trip not exact")

# IMREAD_GRAYSCALE of a color PNG == BT.601 gray
ok, buf = cv2.imencode('.png', img)
gdec = cv2.imdecode(buf, cv2.IMREAD_GRAYSCALE)
g.check(gdec is not None and np.all(np.abs(gdec.astype(int) - gray.astype(int)) <= 1),
        "IMREAD_GRAYSCALE != BT.601 gray")

# imwrite/imread file round-trip
tdir = os.environ.get('TMPDIR', '/tmp')
p = os.path.join(tdir, 'cvio_rt_py.png')
w = cv2.imwrite(p, img)
rd = cv2.imread(p, cv2.IMREAD_COLOR)
g.check(w and rd is not None and np.array_equal(rd, img), "imwrite/imread PNG file round-trip not exact")
if os.path.exists(p):
    os.remove(p)

# VideoWriter + VideoCapture (FFV1 lossless)
vp = os.path.join(tdir, 'cvio_clip_py.avi')
vw = cv2.VideoWriter(vp, cv2.VideoWriter_fourcc(*'FFV1'), 10.0, (32, 32), True)
if not vw.isOpened():
    g.skip("no FFV1 VideoWriter available - video legs skipped")
else:
    for i in range(5):
        fr = np.zeros((32, 32, 3), dtype=np.uint8)
        fr[:, :, 0] = i * 10
        fr[0, 0] = [i, i + 1, i + 2]
        vw.write(fr)
    vw.release()
    cap = cv2.VideoCapture(vp)
    g.check(cap.isOpened(), "VideoCapture failed to open clip")
    cnt = int(cap.get(cv2.CAP_PROP_FRAME_COUNT))
    g.check(cnt == 5, "clip frame count != 5")
    got, f0 = cap.read()
    g.check(got and f0 is not None, "could not read first frame")
    g.check(got and tuple(f0[0, 0]) == (0, 1, 2), "first-frame marker != (0,1,2)")
    g.check(got and abs(int(f0[:, :, 0].mean()) - 0) <= 1, "first-frame B mean != 0")
    cap.release()
    if os.path.exists(vp):
        os.remove(vp)

# Real-asset leg
ad = os.environ.get('ASSET_DIR')
read_assets = 0
if ad and os.path.isdir(ad):
    for n in sorted(os.listdir(ad)):
        if n.lower().endswith(('.png', '.jpg', '.jpeg', '.bmp', '.ppm', '.tiff', '.tif')):
            m = cv2.imread(os.path.join(ad, n), cv2.IMREAD_COLOR)
            g.check(m is not None and m.ndim == 3 and m.shape[2] == 3, "asset failed to decode: %s" % n)
            read_assets += 1
if read_assets == 0:
    g.skip("no images under ASSET_DIR - real-asset leg honest-skipped")

raise SystemExit(g.finish())
