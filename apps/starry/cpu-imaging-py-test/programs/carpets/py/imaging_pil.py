#!/usr/bin/env python3
"""imaging_pil - Pillow (PIL) per-API assertions vs closed-form / numpy goldens.

Every leg drives a real PIL operation on a KNOWN image and asserts the output against a value predicted
from first principles: pixel access, resize (NEAREST block-exact, BILINEAR closed form at derived src
coords), rotate/transpose (90/180 exact index mapping), convert (RGB<->L luma == PIL's L24 fixed-point
ITU-R 601-2 form, RGB<->RGBA), ImageDraw (analytic masks), ImageFilter (impulse-response kernels,
FIND_EDGES), point/eval, format round-trip (lossless byte-exact via np.array, JPEG PSNR), getbbox /
histogram / split / merge.
"""
import io
import numpy as np
from PIL import Image, ImageDraw, ImageFilter, ImageChops

from img_common import Gate, luma_601_round, luma_601_float

g = Gate("IMAGING_PIL")


def data(im):
    return list(im.get_flattened_data())


# ---------------------------------------------------------------- new / pixel access
im = Image.new("RGB", (3, 2), (10, 20, 30))
g.check(im.size == (3, 2) and im.mode == "RGB", "Image.new size/mode wrong")
g.check(im.getpixel((0, 0)) == (10, 20, 30), "solid-fill getpixel wrong")

# putdata a known raster, assert getpixel matches the closed-form index formula
L = Image.new("L", (4, 3))
L.putdata([y * 4 + x for y in range(3) for x in range(4)])  # value == flat index
ok = all(L.getpixel((x, y)) == y * 4 + x for y in range(3) for x in range(4))
g.check(ok, "putdata/getpixel != flat index")
g.check(np.array_equal(np.array(L), np.arange(12).reshape(3, 4)), "np.array(L) != arange raster")

# ---------------------------------------------------------------- resize NEAREST (block-exact)
src = Image.new("L", (2, 2)); src.putdata([0, 100, 200, 255])
near = src.resize((4, 4), Image.NEAREST)
want_near = np.repeat(np.repeat(np.array([[0, 100], [200, 255]], np.uint8), 2, 0), 2, 1)
g.check(np.array_equal(np.array(near), want_near), "NEAREST 2x->4x != block replication")

# ---------------------------------------------------------------- resize BILINEAR (closed form)
# PIL maps dst center to src: c = (dx+0.5)*scale - 0.5, scale = src/dst, clamp to [0, src-1],
# then linear-interpolate neighbours and round half away from zero.
row = [0, 90, 180, 255]
srcL = Image.new("L", (4, 1)); srcL.putdata(row)
bil = srcL.resize((8, 1), Image.BILINEAR)
got = data(bil)
scale = 4.0 / 8.0
want_bil = []
for dx in range(8):
    c = max(0.0, min(3.0, (dx + 0.5) * scale - 0.5))
    x0 = int(np.floor(c)); x1 = min(x0 + 1, 3); frac = c - x0
    v = row[x0] * (1 - frac) + row[x1] * frac
    want_bil.append(int(v + 0.5))
g.check(got == want_bil, "BILINEAR != closed-form linear interp (%r vs %r)" % (got, want_bil))
g.check(got[0] == 0 and got[-1] == 255, "BILINEAR endpoints not clamped to source ends")

# ---------------------------------------------------------------- rotate / transpose (exact mapping)
a = np.arange(6, dtype=np.uint8).reshape(2, 3)  # H=2 W=3
pim = Image.fromarray(a)
g.check(np.array_equal(np.array(pim.transpose(Image.ROTATE_90)), np.rot90(a, 1)),
        "ROTATE_90 != np.rot90 k=1")
g.check(np.array_equal(np.array(pim.transpose(Image.ROTATE_180)), np.rot90(a, 2)),
        "ROTATE_180 != np.rot90 k=2")
g.check(np.array_equal(np.array(pim.transpose(Image.FLIP_LEFT_RIGHT)), a[:, ::-1]),
        "FLIP_LEFT_RIGHT != column reverse")
g.check(np.array_equal(np.array(pim.transpose(Image.FLIP_TOP_BOTTOM)), a[::-1, :]),
        "FLIP_TOP_BOTTOM != row reverse")
g.check(np.array_equal(np.array(pim.transpose(Image.TRANSPOSE)), a.T),
        "TRANSPOSE != a.T")
# rotate(90, expand) is a rigid 90 CCW rotation == np.rot90
g.check(np.array_equal(np.array(pim.rotate(90, expand=True)), np.rot90(a, 1)),
        "rotate(90,expand) != np.rot90")

# ---------------------------------------------------------------- convert RGB<->L (601-2 luma)
rgb = Image.new("RGB", (4, 1))
px = [(255, 0, 0), (0, 255, 0), (0, 0, 255), (123, 231, 45)]
rgb.putdata(px)
Lc = rgb.convert("L")
want_L = [luma_601_round(*p) for p in px]
g.check(data(Lc) == want_L, "RGB->L != L24 601-2 closed form (%r vs %r)" % (data(Lc), want_L))
# each within 1 LSB of the real-valued 601-2 luma
g.check(all(abs(data(Lc)[i] - luma_601_float(*px[i])) <= 1.0 for i in range(4)),
        "RGB->L drifts >1 from real 601-2 luma")
# pins: pure red/green/blue
g.check(data(Lc)[:3] == [76, 150, 29], "601-2 primaries luma != (76,150,29)")

# RGB<->RGBA: adding alpha == opaque, dropping alpha preserves RGB
rgba = rgb.convert("RGBA")
g.check(rgba.mode == "RGBA" and rgba.getpixel((0, 0)) == (255, 0, 0, 255),
        "RGB->RGBA alpha != 255")
g.check(np.array_equal(np.array(rgba.convert("RGB")), np.array(rgb)),
        "RGBA->RGB dropped/altered color")

# ---------------------------------------------------------------- ImageDraw analytic masks
canvas = Image.new("L", (20, 20), 0)
d = ImageDraw.Draw(canvas)
d.rectangle([5, 5, 14, 12], fill=255)  # inclusive corners: 10 wide x 8 tall
arr = np.array(canvas)
inside = arr[5:13, 5:15]
g.check(np.all(inside == 255) and int((arr == 255).sum()) == 10 * 8,
        "filled rectangle area/coverage != 10x8 analytic mask")

canvas2 = Image.new("L", (10, 10), 0)
ImageDraw.Draw(canvas2).line([0, 0, 9, 0], fill=255, width=1)  # top row
g.check(np.array_equal(np.array(canvas2)[0], np.full(10, 255, np.uint8)) and
        int((np.array(canvas2) == 255).sum()) == 10, "horizontal line != exact top row")

canvas3 = Image.new("L", (5, 5), 0)
ImageDraw.Draw(canvas3).line([0, 0, 4, 4], fill=255, width=1)  # main diagonal
g.check(np.array_equal(np.diag(np.array(canvas3)), np.full(5, 255, np.uint8)),
        "diagonal line != exact main diagonal")

# filled ellipse == disc: coverage within +/-15% of pi r^2 and center lit, corners dark
canvas4 = Image.new("L", (21, 21), 0)
ImageDraw.Draw(canvas4).ellipse([1, 1, 19, 19], fill=255)  # r=9 centered at 10
a4 = np.array(canvas4)
area = int((a4 == 255).sum()); expect = np.pi * 9 * 9
g.check(abs(area - expect) / expect < 0.15, "filled ellipse area off pi r^2 by >15%%")
g.check(a4[10, 10] == 255 and a4[0, 0] == 0 and a4[20, 20] == 0,
        "ellipse center not lit or corners lit")

# polygon triangle: an interior point lit, an exterior point dark
canvas5 = Image.new("L", (10, 10), 0)
ImageDraw.Draw(canvas5).polygon([(0, 0), (9, 0), (0, 9)], fill=255)
a5 = np.array(canvas5)
g.check(a5[1, 1] == 255 and a5[8, 8] == 0, "triangle polygon interior/exterior wrong")

# text: ink stays inside its bbox and produces some ink (bitmap font is built in)
tcanvas = Image.new("L", (60, 16), 0)
td = ImageDraw.Draw(tcanvas)
td.text((1, 1), "Ax", fill=255)
ta = np.array(tcanvas)
ink = ta > 0
g.check(ink.any(), "text drew no ink")
ys, xs = np.where(ink)
g.check(xs.min() >= 1 and ys.min() >= 1 and xs.max() < 60 and ys.max() < 16,
        "text ink escaped its drawing box")

# ---------------------------------------------------------------- ImageFilter impulse responses
# BLUR is a normalized 5x5 box-ish kernel; on a constant field it is identity.
flat = Image.new("L", (9, 9), 100)
g.check(np.array_equal(np.array(flat.filter(ImageFilter.BLUR))[3:6, 3:6],
                       np.full((3, 3), 100, np.uint8)),
        "BLUR of constant field != constant (interior)")
# GaussianBlur preserves a constant field exactly and conserves brightness of an impulse
imp = np.zeros((11, 11), np.float64); imp[5, 5] = 255.0
impimg = Image.fromarray(imp.astype(np.uint8))
gb = np.array(impimg.filter(ImageFilter.GaussianBlur(radius=2)))
g.check(gb[5, 5] > 0 and gb[5, 5] < 255 and gb[0, 0] == 0,
        "GaussianBlur impulse: center not spread / far tap nonzero")
g.check(np.array_equal(np.array(flat.filter(ImageFilter.GaussianBlur(radius=2)))[4:6, 4:6],
                       np.full((2, 2), 100, np.uint8)),
        "GaussianBlur of constant field != constant")
# FIND_EDGES: a solid field has no edges (interior stays 0)
solid = Image.new("L", (9, 9), 200)
fe = np.array(solid.filter(ImageFilter.FIND_EDGES))
g.check(fe[3:6, 3:6].sum() == 0, "FIND_EDGES of solid field lit interior")
# a vertical step edge lights the boundary column
step = np.zeros((9, 9), np.uint8); step[:, 5:] = 255
fe2 = np.array(Image.fromarray(step).filter(ImageFilter.FIND_EDGES))
g.check(fe2[4, 4] > 0 or fe2[4, 5] > 0, "FIND_EDGES missed the vertical step")

# ---------------------------------------------------------------- point / eval
ramp = Image.new("L", (256, 1)); ramp.putdata(list(range(256)))
inv = ramp.point(lambda v: 255 - v)
g.check(data(inv) == list(range(255, -1, -1)), "point(255-v) != inverse ramp")
from PIL import ImageMath  # noqa: F401  (ensure PIL image math available even if unused)
half = Image.eval(ramp, lambda v: v // 2)
g.check(data(half) == [v // 2 for v in range(256)], "Image.eval(v//2) wrong")

# ---------------------------------------------------------------- format round-trips
def roundtrip_lossless(mode, arr, fmt):
    im0 = Image.fromarray(arr, mode)
    buf = io.BytesIO(); im0.save(buf, format=fmt); buf.seek(0)
    im1 = Image.open(buf)
    return np.array_equal(np.array(im1.convert(mode)), arr)


np.random.seed(0x233)
rgb_arr = (np.random.rand(8, 8, 3) * 255).astype(np.uint8)
gray_arr = (np.random.rand(8, 8) * 255).astype(np.uint8)
g.check(roundtrip_lossless("RGB", rgb_arr, "PNG"), "PNG RGB round-trip not byte-exact")
g.check(roundtrip_lossless("RGB", rgb_arr, "BMP"), "BMP RGB round-trip not byte-exact")
g.check(roundtrip_lossless("RGB", rgb_arr, "PPM"), "PPM RGB round-trip not byte-exact")
g.check(roundtrip_lossless("RGB", rgb_arr, "TIFF"), "TIFF RGB round-trip not byte-exact")
g.check(roundtrip_lossless("L", gray_arr, "PNG"), "PNG L round-trip not byte-exact")
# GIF is palette-based: round-trip through P mode is exact for <=256 distinct colors
palimg = Image.fromarray(gray_arr, "L").convert("P")
buf = io.BytesIO(); palimg.save(buf, format="GIF"); buf.seek(0)
g.check(np.array_equal(np.array(Image.open(buf).convert("L")), gray_arr),
        "GIF palette round-trip not exact for grayscale")
# JPEG is lossy: on a smooth gradient (the realistic case) q95 must clear a PSNR floor.
yy, xx = np.mgrid[0:32, 0:32]
smooth = np.stack([(xx * 8) % 256, (yy * 8) % 256, ((xx + yy) * 4) % 256], -1).astype(np.uint8)
buf = io.BytesIO(); Image.fromarray(smooth, "RGB").save(buf, format="JPEG", quality=95); buf.seek(0)
jpg = np.array(Image.open(buf).convert("RGB")).astype(np.float64)
mse = np.mean((jpg - smooth.astype(np.float64)) ** 2)
psnr = 10 * np.log10(255.0 ** 2 / mse) if mse > 0 else 99.0
g.check(psnr > 30.0, "JPEG q95 PSNR %.1f below floor" % psnr)

# ---------------------------------------------------------------- getbbox / histogram / split / merge
bb = Image.new("L", (10, 10), 0)
ImageDraw.Draw(bb).rectangle([2, 3, 6, 7], fill=255)
g.check(bb.getbbox() == (2, 3, 7, 8), "getbbox != tight box (exclusive lower-right)")
# histogram: a field of exactly N pixels at value v has count N at bin v
h = Image.new("L", (10, 10), 42).histogram()
g.check(h[42] == 100 and sum(h) == 100, "histogram of constant field wrong")
# split/merge round-trips an RGB image exactly
rr = Image.fromarray(rgb_arr, "RGB")
r_, g_, b_ = rr.split()
merged = Image.merge("RGB", (r_, g_, b_))
g.check(np.array_equal(np.array(merged), rgb_arr), "split/merge RGB not identity")
g.check(np.array_equal(np.array(r_), rgb_arr[:, :, 0]), "split R channel != array[...,0]")

# ImageChops difference of an image with itself is all-zero (sanity of pixel arithmetic)
g.check(np.array(ImageChops.difference(rr, rr)).sum() == 0, "ImageChops.difference(self,self) != 0")

raise SystemExit(g.finish())
