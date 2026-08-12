#!/usr/bin/env python3
"""imaging_skimage - scikit-image per-API assertions vs numpy closed form.

Every leg drives real scikit-image on a KNOWN array and asserts against a numpy closed form:
color.rgb2gray == 0.2125R+0.7154G+0.0721B (skimage's own BT.709 luma, NOT PIL's 601-2); rgb2hsv/lab
round-trip; filters.gaussian (impulse -> unit-sum spread), filters.sobel (linear ramp -> constant),
threshold_otsu (bimodal -> separates the classes); transform.resize/rescale/rotate/warp (closed form at
known points); morphology.dilation/erosion/opening/closing/binary on a known pattern; feature.canny (step
edge localized at the boundary), corner_harris/corner_peaks (checkerboard intersections); measure.label /
regionprops (known blobs -> known area/centroid/bbox); exposure.rescale_intensity/histogram; util
img_as_float/ubyte exact dtype conversions.
"""
import warnings
import numpy as np

warnings.simplefilter("ignore")  # skimage version churn deprecates footprint/binary_* names

from skimage import color, filters, transform, morphology, feature, measure, exposure, util
from img_common import Gate, SK_R, SK_G, SK_B, sk_gray

g = Gate("IMAGING_SKIMAGE")
np.random.seed(0x233)

# ---------------------------------------------------------------- color.rgb2gray (BT.709 luma)
rgb = np.random.rand(6, 6, 3)
gray = color.rgb2gray(rgb)
want = sk_gray(rgb)
g.check(np.allclose(gray, want, atol=1e-12), "rgb2gray != 0.2125R+0.7154G+0.0721B closed form")
# skimage luma differs from PIL 601-2: pure green weight is 0.7154 here vs 0.587 there
g.check(abs(SK_G - 0.7154) < 1e-9 and abs(SK_R - 0.2125) < 1e-9,
        "skimage luma coefficients drifted from BT.709")
g.check(abs(color.rgb2gray(np.array([[[0.0, 1.0, 0.0]]]))[0, 0] - 0.7154) < 1e-9,
        "rgb2gray(pure green) != 0.7154")

# rgb2hsv / hsv2rgb round-trip
g.check(np.allclose(color.hsv2rgb(color.rgb2hsv(rgb)), rgb, atol=1e-10),
        "rgb->hsv->rgb not identity")
# rgb2lab / lab2rgb round-trip
g.check(np.allclose(color.lab2rgb(color.rgb2lab(rgb)), rgb, atol=1e-8),
        "rgb->lab->rgb not identity")
# HSV of pure red: hue 0, sat 1, val 1
hsv_red = color.rgb2hsv(np.array([[[1.0, 0.0, 0.0]]]))[0, 0]
g.check(abs(hsv_red[0]) < 1e-9 and abs(hsv_red[1] - 1) < 1e-9 and abs(hsv_red[2] - 1) < 1e-9,
        "rgb2hsv(red) != (0,1,1)")

# ---------------------------------------------------------------- filters.gaussian (impulse)
imp = np.zeros((11, 11)); imp[5, 5] = 1.0
gg = filters.gaussian(imp, sigma=1.0)
g.check(abs(gg.sum() - 1.0) < 1e-6, "gaussian(impulse) does not conserve mass (sum!=1)")
g.check(gg[5, 5] == gg.max() and gg[5, 5] < 1.0 and gg[0, 0] < gg[5, 5],
        "gaussian(impulse) peak not at center / not spread")
# a constant field is a gaussian fixed point
flat = np.full((11, 11), 0.4)
g.check(np.allclose(filters.gaussian(flat, sigma=1.0), 0.4, atol=1e-6),
        "gaussian of constant field != constant")

# ---------------------------------------------------------------- filters.sobel (linear ramp)
ramp = np.tile(np.arange(10, dtype=float), (10, 1))  # slope 1 along x
sob = filters.sobel(ramp)
g.check(np.allclose(sob[2:8, 2:8], np.sqrt(2.0), atol=1e-9),
        "sobel(unit-slope ramp) interior != sqrt(2)")
# sobel_v recovers the pure horizontal derivative == 2 (skimage's 1/4-normalized kernel * slope-scaling)
sob_v = filters.sobel_v(ramp)
g.check(np.allclose(sob_v[2:8, 2:8], 2.0, atol=1e-9), "sobel_v(unit-slope ramp) interior != 2")
# a constant field has zero gradient
g.check(np.allclose(filters.sobel(np.full((8, 8), 5.0)), 0.0, atol=1e-9),
        "sobel of constant field != 0")

# ---------------------------------------------------------------- threshold_otsu (bimodal)
bim = np.zeros((40, 40), np.uint8); bim[:20] = 30; bim[20:] = 220
t = filters.threshold_otsu(bim)
seg = bim > t
g.check(30 <= t < 220, "otsu threshold not between the two modes")
g.check(seg[:20].sum() == 0 and seg[20:].sum() == 20 * 40,
        "otsu threshold does not separate the two classes")

# ---------------------------------------------------------------- transform.resize / rescale / rotate
src = np.array([[0.0, 100.0], [200.0, 255.0]])
res0 = transform.resize(src, (4, 4), order=0, anti_aliasing=False)  # nearest -> 2x2 block replication
want_block = np.repeat(np.repeat(src, 2, 0), 2, 1)
g.check(np.array_equal(res0, want_block), "resize order0 != block replication")
rs = transform.rescale(src, 2, order=0, anti_aliasing=False)
g.check(rs.shape == (4, 4) and np.array_equal(rs, want_block), "rescale 2x != resize (4,4)")
# rotate 90 CCW of a 2x2 == np.rot90
rot = transform.rotate(src, 90, resize=True, order=0)
g.check(np.allclose(rot, np.rot90(src, 1), atol=1e-9), "rotate(90) != np.rot90")
# warp with an identity affine matrix is a no-op
tf_id = transform.AffineTransform(matrix=np.eye(3))
g.check(np.allclose(transform.warp(src, tf_id, order=1), src, atol=1e-9),
        "warp(identity) != source")
# an integer translation shifts content by exact pixels
# AffineTransform.translation is applied in the forward map; warp inverts it, so translation=(1,0)
# samples output col j from input col j+1 (content slides one column left).
tf_sh = transform.AffineTransform(translation=(1, 0))
img = np.arange(16, dtype=float).reshape(4, 4)
shifted = transform.warp(img, tf_sh, order=0)
g.check(np.array_equal(shifted[:, :3], img[:, 1:4]), "warp integer translation != exact pixel shift")

# ---------------------------------------------------------------- morphology on a known pattern
fp = morphology.footprint_rectangle((3, 3))
dot = np.zeros((7, 7), bool); dot[3, 3] = True
dil = morphology.dilation(dot, fp)
g.check(dil.sum() == 9 and np.all(dil[2:5, 2:5]), "dilation(dot, 3x3) != 9-cell block")
g.check(morphology.erosion(dil, fp).sum() == 1, "erosion(dilation) does not recover the dot")
speck = np.zeros((9, 9), bool); speck[4, 4] = True
g.check(morphology.opening(speck, fp).sum() == 0, "opening did not remove a 1px speck")
solid = np.ones((9, 9), bool); solid[4, 4] = False
g.check(morphology.closing(solid, fp).sum() == 81, "closing did not fill a 1px hole")
# grayscale erosion <= image <= dilation everywhere
gimg = np.arange(25, dtype=np.uint8).reshape(5, 5)
g.check(np.all(morphology.erosion(gimg, fp) <= gimg) and np.all(morphology.dilation(gimg, fp) >= gimg),
        "grayscale erosion/dilation violate the ordering bound")

# ---------------------------------------------------------------- feature.canny / corners
step = np.zeros((20, 20), float); step[:, 10:] = 1.0
edges = feature.canny(step, sigma=1.0)
cols = set(np.where(edges)[1].tolist())
g.check(len(cols) > 0 and cols.issubset({9, 10, 11}), "canny step edge not localized at the boundary")
g.check(edges[:, :7].sum() == 0 and edges[:, 13:].sum() == 0, "canny lit pixels far from the edge")
# checkerboard corners
cb = (np.indices((8, 8)).sum(0) % 2).astype(float)
harris = feature.corner_harris(cb)
peaks = feature.corner_peaks(harris, min_distance=1)
g.check(len(peaks) >= 1, "corner_harris/corner_peaks found no checkerboard corners")

# ---------------------------------------------------------------- measure.label / regionprops
blobs = np.zeros((10, 10), bool)
blobs[1:4, 1:4] = True   # 3x3 = area 9, centroid (2,2), bbox (1,1,4,4)
blobs[6:8, 6:9] = True   # 2x3 = area 6, centroid (6.5,7), bbox (6,6,8,9)
lbl = measure.label(blobs)
g.check(lbl.max() == 2, "label did not find exactly 2 connected components")
props = {p.area: p for p in measure.regionprops(lbl)}
g.check(9 in props and 6 in props, "regionprops areas != {9,6}")
p9 = props[9]
g.check(tuple(np.round(p9.centroid, 6)) == (2.0, 2.0), "3x3 blob centroid != (2,2)")
g.check(p9.bbox == (1, 1, 4, 4), "3x3 blob bbox != (1,1,4,4)")
p6 = props[6]
g.check(tuple(np.round(p6.centroid, 6)) == (6.5, 7.0), "2x3 blob centroid != (6.5,7)")

# ---------------------------------------------------------------- exposure
lin = np.array([50.0, 100.0, 150.0, 200.0])
rsc = exposure.rescale_intensity(lin, out_range=(0.0, 1.0))
g.check(np.allclose(rsc, (lin - 50) / 150.0), "rescale_intensity != affine map to [0,1]")
hist, centers = exposure.histogram(np.full((10, 10), 7, np.uint8))
g.check(hist[np.where(centers == 7)[0][0]] == 100 and hist.sum() == 100,
        "histogram of constant field wrong")

# ---------------------------------------------------------------- util dtype conversions (exact)
g.check(np.array_equal(util.img_as_float(np.array([0, 255], np.uint8)), np.array([0.0, 1.0])),
        "img_as_float(ubyte) endpoints != 0/1")
g.check(np.array_equal(util.img_as_ubyte(np.array([0.0, 1.0])), np.array([0, 255], np.uint8)),
        "img_as_ubyte(float) endpoints != 0/255")
g.check(util.img_as_float(np.array([128], np.uint8))[0] == 128 / 255.0,
        "img_as_float(128) != 128/255")

raise SystemExit(g.finish())
