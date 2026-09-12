/* gui_common.h - shared primitives for the cpu-gui-qt-test carpet (a "pyte for GUI widgets").
 *
 * Each cell drives a real Qt Widgets / QPainter pipeline on the CPU RASTER paint engine with the
 * offscreen QPA platform plugin (QT_QPA_PLATFORM=offscreen) - NO GPU, NO display server - and asserts the
 * result against a CLOSED-FORM golden: exact per-pixel colors from known fillRect/drawLine/drawEllipse
 * geometry, Porter-Duff "over" alpha compositing computed by hand, exact layout geometry() from the
 * QVBoxLayout/QHBoxLayout/QGridLayout math, and post-event widget state from injected QTest events.
 * "Widget created" alone is NOT a test here - every leg checks a value it can predict from first principles.
 *
 * Determinism: fixed widget sizes, ARGB32 images (no premultiply surprises where avoidable), a fixed RNG
 * seed (0x233) where any randomness would appear, and the raster engine (pure CPU) so pixels are identical
 * across arch. Text legs assert an ink bounding-box + non-empty coverage (the exact glyph pixels depend on
 * the bundled font, so we never assert glyph-exact pixels - only that ink lands where it must and nowhere
 * it must not).
 *
 * Three-gate marker: a cell prints "GUI_<CELL> OK <n>" only when fail==0 && total==pass && total>0.
 */
#ifndef GUI_COMMON_H
#define GUI_COMMON_H

#include <QImage>
#include <QColor>
#include <QPainter>
#include <QString>
#include <cstdio>
#include <cstdlib>

/* -------- three-gate marker (identical semantics to the subtitle/model/font carpets) -------- */
struct Gate {
    int pass = 0, total = 0, fail = 0;
    const char *name;
    explicit Gate(const char *n) : name(n) {}
    void check(bool cond, const char *msg) {
        total++;
        if (cond) pass++;
        else { fail++; fprintf(stderr, "  FAIL: %s\n", msg); }
    }
    int finish() {
        if (fail == 0 && total == pass && total > 0) {
            printf("%s OK %d\n", name, total);
            return 0;
        }
        printf("%s FAILED pass=%d total=%d fail=%d\n", name, pass, total, fail);
        return 1;
    }
};

/* -------- pixel helpers over QImage (all work on ARGB32 so a pixel is a plain 0xAARRGGBB) -------- */

/* channel-wise |a-b| <= tol for every channel including alpha */
static inline bool argb_close(QRgb a, QRgb b, int tol) {
    return qAbs(qAlpha(a) - qAlpha(b)) <= tol &&
           qAbs(qRed(a)   - qRed(b))   <= tol &&
           qAbs(qGreen(a) - qGreen(b)) <= tol &&
           qAbs(qBlue(a)  - qBlue(b))  <= tol;
}

/* Assert every pixel inside [x0,x1) x [y0,y1) is within tol of want. Returns true if ALL match. */
static inline bool rect_is_color(const QImage &img, int x0, int y0, int x1, int y1, QRgb want, int tol) {
    for (int y = y0; y < y1; ++y)
        for (int x = x0; x < x1; ++x)
            if (!argb_close(img.pixel(x, y), want, tol)) return false;
    return true;
}

/* Count pixels inside the rect that are within tol of want. */
static inline int count_color(const QImage &img, int x0, int y0, int x1, int y1, QRgb want, int tol) {
    int n = 0;
    for (int y = y0; y < y1; ++y)
        for (int x = x0; x < x1; ++x)
            if (argb_close(img.pixel(x, y), want, tol)) ++n;
    return n;
}

/* Count pixels in the whole image that differ from bg by more than tol on any channel ("ink"). */
static inline int count_non_bg(const QImage &img, QRgb bg, int tol) {
    int n = 0;
    for (int y = 0; y < img.height(); ++y)
        for (int x = 0; x < img.width(); ++x)
            if (!argb_close(img.pixel(x, y), bg, tol)) ++n;
    return n;
}

/* Tight bounding box of pixels differing from bg by more than tol. Returns false if no ink at all. */
static inline bool ink_bbox(const QImage &img, QRgb bg, int tol,
                            int *minx, int *miny, int *maxx, int *maxy) {
    int lx = img.width(), ly = img.height(), hx = -1, hy = -1;
    for (int y = 0; y < img.height(); ++y)
        for (int x = 0; x < img.width(); ++x)
            if (!argb_close(img.pixel(x, y), bg, tol)) {
                if (x < lx) lx = x; if (x > hx) hx = x;
                if (y < ly) ly = y; if (y > hy) hy = y;
            }
    if (hx < 0) return false;
    *minx = lx; *miny = ly; *maxx = hx; *maxy = hy;
    return true;
}

/* Porter-Duff "source over destination" for straight (non-premultiplied) ARGB, per channel.
 * out = src + dst*(1-src_a).  Alphas/channels are 0..255. This is the closed form QPainter's default
 * CompositionMode_SourceOver produces; we compare the rendered pixel against this. */
static inline int pd_over_chan(int src_c, int src_a, int dst_c, int dst_a) {
    /* result alpha */
    double sa = src_a / 255.0, da = dst_a / 255.0;
    double oa = sa + da * (1.0 - sa);
    if (oa <= 0.0) return 0;
    /* straight-alpha over: (src_c*sa + dst_c*da*(1-sa)) / oa */
    double oc = (src_c * sa + dst_c * da * (1.0 - sa)) / oa;
    int v = (int)(oc + 0.5);
    return v < 0 ? 0 : (v > 255 ? 255 : v);
}
static inline int pd_over_alpha(int src_a, int dst_a) {
    double sa = src_a / 255.0, da = dst_a / 255.0;
    double oa = sa + da * (1.0 - sa);
    int v = (int)(oa * 255.0 + 0.5);
    return v < 0 ? 0 : (v > 255 ? 255 : v);
}

/* A fresh ARGB32 image flood-filled with bg. */
static inline QImage new_image(int w, int h, QRgb bg) {
    QImage img(w, h, QImage::Format_ARGB32);
    img.fill(bg);
    return img;
}

#endif /* GUI_COMMON_H */
