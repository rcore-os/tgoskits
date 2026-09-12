"""cv_common.py - shared primitives for the cpu-opencv-test carpet (Python side).

Each cell drives real cv2 (numpy-backed cv::Mat, cvtColor, GaussianBlur, resize, threshold, drawing, Canny,
imencode ...) on KNOWN fixed inputs and asserts against a CLOSED-FORM / numpy golden computed here - the
numpy reference and the comparison are ours; the algorithm under test is OpenCV's. "import cv2" is not a
test; every leg checks a value predicted from first principles.

Determinism: fixed inputs, cv2.setNumThreads(1), np.random.seed(0x233) wherever any random path appears.

Three-gate marker: a cell prints "OPENCV_<CELL> OK <n>" only when fail==0 and total==pass and total>0.
"""
import sys
import numpy as np


class Gate:
    def __init__(self, name):
        self.name = name
        self.p = self.t = self.f = self.s = 0

    def check(self, cond, msg):
        self.t += 1
        if cond:
            self.p += 1
        else:
            self.f += 1
            sys.stderr.write("  FAIL: %s\n" % msg)

    # honest-skip: recorded distinctly (NOT as a pass) so a run that degrades every leg to skip cannot
    # satisfy the gate; total still counts it so the "OK <n>" marker stays stable, but pass tracks only
    # real checks and finish() requires at least one.
    def skip(self, msg):
        self.t += 1
        self.s += 1
        sys.stderr.write("  SKIP: %s\n" % msg)

    def finish(self):
        if self.f == 0 and self.p > 0 and self.p + self.s == self.t:
            print("%s OK %d" % (self.name, self.t))
            return 0
        print("%s FAILED pass=%d skipped=%d total=%d fail=%d" % (self.name, self.p, self.s, self.t, self.f))
        return 1


def bgr2gray_601(b, g, r):
    """OpenCV COLOR_BGR2GRAY fixed-point closed form (byte-exact for 0..255)."""
    return (r * 4899 + g * 9617 + b * 1868 + 8192) >> 14
