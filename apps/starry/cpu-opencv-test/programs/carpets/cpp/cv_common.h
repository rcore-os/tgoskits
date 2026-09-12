/* cv_common.h - shared primitives for the cpu-opencv-test carpet (C++ side).
 *
 * Each cell drives real OpenCV (cv::Mat / cvtColor / GaussianBlur / resize / threshold / drawing / Canny /
 * imencode ...) on KNOWN, fixed inputs and asserts the result against a CLOSED-FORM golden computed here by
 * hand (Gray = 0.299R+0.587G+0.114B, Porter-Duff, bilinear interpolation, the normalized Gaussian kernel,
 * a Sobel gradient's constant derivative, an analytic drawn shape, a known step-edge column, a PNG/BMP
 * round-trip that must be byte-exact). "cv2 loaded" is NOT a test - every leg checks a value predicted from
 * first principles or a numpy/host-calibrated golden.
 *
 * Determinism: fixed inputs, no threading surprises (cv::setNumThreads(1)), a fixed RNG seed (0x233) wherever
 * a random path could appear. Pixels/values are identical across arch (all integer or IEEE-754 CPU math).
 *
 * Three-gate marker: a cell prints "OPENCV_<CELL> OK <n>" only when fail==0 && total==pass && total>0.
 */
#ifndef CV_COMMON_H
#define CV_COMMON_H

#include <opencv2/core.hpp>
#include <cstdio>
#include <cstdlib>
#include <cmath>
#include <string>

struct Gate {
    int pass = 0, total = 0, fail = 0, skipped = 0;
    const char *name;
    explicit Gate(const char *n) : name(n) {}
    void check(bool cond, const char *msg) {
        total++;
        if (cond) pass++;
        else { fail++; fprintf(stderr, "  FAIL: %s\n", msg); }
    }
    /* honest-skip: recorded distinctly (NOT as a pass) so a run that degrades every leg to skip cannot
       satisfy the gate; total still counts it so the "OK <n>" marker stays stable, but pass tracks only
       real checks and the gate below requires at least one. */
    void skip(const char *msg) { total++; skipped++; fprintf(stderr, "  SKIP: %s\n", msg); }
    int finish() {
        if (fail == 0 && pass > 0 && pass + skipped == total) {
            printf("%s OK %d\n", name, total);
            return 0;
        }
        printf("%s FAILED pass=%d skipped=%d total=%d fail=%d\n", name, pass, skipped, total, fail);
        return 1;
    }
};

/* BT.601 luma round used by cv::cvtColor(...COLOR_BGR2GRAY): Y = round(0.299R + 0.587G + 0.114B).
 * OpenCV uses fixed-point (R*4899 + G*9617 + B*1868 + 8192) >> 14; that equals the rounded float form for
 * all 0..255 inputs, so we assert against the fixed-point closed form to be byte-exact. */
static inline int bgr2gray_601(int b, int g, int r) {
    return (r * 4899 + g * 9617 + b * 1868 + 8192) >> 14;
}

/* Straight-alpha Porter-Duff "src over dst", per channel, 0..255 (used to reason about addWeighted etc.). */
static inline int over_chan(int sc, int sa, int dc, int da) {
    double s = sa / 255.0, d = da / 255.0, oa = s + d * (1.0 - s);
    if (oa <= 0) return 0;
    int v = (int)((sc * s + dc * d * (1.0 - s)) / oa + 0.5);
    return v < 0 ? 0 : (v > 255 ? 255 : v);
}

static inline bool close_i(int a, int b, int tol) { return std::abs(a - b) <= tol; }
static inline bool close_d(double a, double b, double tol) { return std::fabs(a - b) <= tol; }

#endif /* CV_COMMON_H */
