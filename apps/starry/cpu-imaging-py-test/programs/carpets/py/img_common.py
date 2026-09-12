"""img_common.py - shared primitives for the cpu-imaging-py-test carpet.

Each cell drives a REAL imaging library (Pillow / imageio / scikit-image) on KNOWN, fixed inputs and
asserts every result against a CLOSED-FORM / numpy golden computed here. The numpy reference and the
comparison are ours; the algorithm under test belongs to the library. "import PIL"/"imread succeeded"
is NOT a test - every leg checks a value predicted from first principles or a calibrated numpy golden.

Determinism: fixed inputs everywhere; np.random.seed(0x233) wherever any RNG path could appear.

Three-gate marker: a cell prints "IMAGING_<CELL> OK <n>" only when fail==0 and total==pass and total>0;
otherwise "IMAGING_<CELL> FAILED pass=.. total=.. fail=.." and a non-zero exit. run_all.sh gates the whole
carpet on the expected_cells manifest (fail==0 && total==EXPECTED==pass, EXPECTED>=1 floor).
"""
import sys
import numpy as np


class Gate:
    def __init__(self, name):
        self.name = name
        self.p = self.t = self.f = 0

    def check(self, cond, msg):
        self.t += 1
        if bool(cond):
            self.p += 1
        else:
            self.f += 1
            sys.stderr.write("  FAIL: %s\n" % msg)

    def fail(self, msg):
        # a leg that should have run but could not - the gate must not pass
        self.t += 1
        self.f += 1
        sys.stderr.write("  FAIL: %s\n" % msg)

    def finish(self):
        if self.f == 0 and self.t == self.p and self.t > 0:
            print("%s OK %d" % (self.name, self.t))
            return 0
        print("%s FAILED pass=%d total=%d fail=%d" % (self.name, self.p, self.t, self.f))
        return 1


def luma_601_round(r, g, b):
    """PIL RGB->L per ITU-R 601-2: L = R*299/1000 + G*587/1000 + B*114/1000, then rounded.

    Pillow computes L = (R*299 + G*587 + B*114) / 1000 and truncates (int()), which for the integer
    accumulator equals floor((R*299 + G*587 + B*114 + 500) / 1000)? No - Pillow uses L24 fixed point:
    L = (R*19595 + G*38470 + B*7471 + 0x8000) >> 16. That is the byte-exact closed form we assert against.
    """
    return (int(r) * 19595 + int(g) * 38470 + int(b) * 7471 + 0x8000) >> 16


def luma_601_float(r, g, b):
    """The documented ITU-R 601-2 real-valued luma (for reference / <=1 LSB cross-checks)."""
    return r * 299.0 / 1000.0 + g * 587.0 / 1000.0 + b * 114.0 / 1000.0


# scikit-image rgb2gray coefficients (Y' of ITU-R BT.709), operating on float [0,1] images.
SK_R, SK_G, SK_B = 0.2125, 0.7154, 0.0721


def sk_gray(rgb_float):
    """scikit-image color.rgb2gray closed form: 0.2125 R + 0.7154 G + 0.0721 B on float images."""
    rgb_float = np.asarray(rgb_float, dtype=np.float64)
    return rgb_float[..., 0] * SK_R + rgb_float[..., 1] * SK_G + rgb_float[..., 2] * SK_B


def gaussian_kernel_1d(sigma, radius):
    """Normalized 1-D Gaussian taps, the textbook reference for a separable Gaussian blur."""
    x = np.arange(-radius, radius + 1, dtype=np.float64)
    k = np.exp(-(x * x) / (2.0 * sigma * sigma))
    return k / k.sum()
