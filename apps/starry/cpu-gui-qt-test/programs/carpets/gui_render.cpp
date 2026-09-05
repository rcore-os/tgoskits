/* gui_render.cpp - per-pixel widget rendering vs closed form on Qt's CPU raster paint engine.
 *
 * Every leg draws with KNOWN geometry into an ARGB32 QImage (or grabs a real QWidget to a QImage) and then
 * asserts the exact pixels:
 *   - fillRect(x,y,w,h,color) -> those pixels are exactly `color`, the surrounding background is untouched.
 *   - drawLine (axis-aligned + the diagonal endpoints) -> the drawn pixels carry the pen color.
 *   - drawEllipse (a filled circle) -> analytic coverage: inside-radius pixels are fill, far-outside pixels
 *     are background, and the total covered-pixel count is within tolerance of pi*r^2.
 *   - alpha compositing of a semi-transparent rect over an opaque one -> Porter-Duff "over" per pixel.
 *   - a real QLabel grabbed to a QImage -> its palette background fills the widget and it carries ink.
 *   - text: draw one glyph, assert ink lands inside the expected bounding box and the rest stays background
 *     (glyph-exact pixels depend on the font, so we assert bbox + coverage, never the literal glyph shape).
 *
 * QT_QPA_PLATFORM=offscreen: no display server. Raster engine: pure CPU. Deterministic across arch.
 */
#include "gui_common.h"
#include <QApplication>
#include <QLabel>
#include <QPushButton>
#include <QFont>
#include <QPen>
#include <QBrush>
#include <cmath>

/* named colors as straight ARGB (0xAARRGGBB) */
static const QRgb BG    = qRgba(0x20, 0x20, 0x20, 0xFF); /* dark gray, opaque */
static const QRgb RED   = qRgba(0xFF, 0x00, 0x00, 0xFF);
static const QRgb GREEN = qRgba(0x00, 0xFF, 0x00, 0xFF);
static const QRgb BLUE  = qRgba(0x00, 0x00, 0xFF, 0xFF);

/* ---- fillRect: exact pixels inside, exact background outside ---- */
static void leg_fillrect(Gate &g) {
    QImage img = new_image(100, 80, BG);
    QPainter p(&img);
    p.setRenderHint(QPainter::Antialiasing, false);
    p.fillRect(20, 15, 40, 30, QColor(RED)); /* x=[20,60) y=[15,45) */
    p.end();

    g.check(rect_is_color(img, 20, 15, 60, 45, RED, 0), "fillRect: interior not exact red");
    /* the four background bands around the rect must be untouched */
    g.check(rect_is_color(img, 0, 0, 100, 15, BG, 0),  "fillRect: top band disturbed");
    g.check(rect_is_color(img, 0, 45, 100, 80, BG, 0), "fillRect: bottom band disturbed");
    g.check(rect_is_color(img, 0, 15, 20, 45, BG, 0),  "fillRect: left band disturbed");
    g.check(rect_is_color(img, 60, 15, 100, 45, BG, 0),"fillRect: right band disturbed");
    /* corner just inside is red, corner just outside is bg (closed-form edge) */
    g.check(argb_close(img.pixel(20, 15), RED, 0), "fillRect: top-left inside pixel wrong");
    g.check(argb_close(img.pixel(59, 44), RED, 0), "fillRect: bottom-right inside pixel wrong");
    g.check(argb_close(img.pixel(19, 15), BG, 0),  "fillRect: pixel left of edge not bg");
    g.check(argb_close(img.pixel(60, 15), BG, 0),  "fillRect: pixel right of edge not bg");
    /* exact covered-pixel count = 40*30 */
    g.check(count_color(img, 0, 0, 100, 80, RED, 0) == 40 * 30, "fillRect: red pixel count != 1200");
}

/* ---- drawLine: axis-aligned lines land on their exact rows/cols ---- */
static void leg_drawline(Gate &g) {
    QImage img = new_image(60, 60, BG);
    QPainter p(&img);
    p.setRenderHint(QPainter::Antialiasing, false);
    QPen pen{QColor(GREEN)};
    pen.setWidth(1);
    p.setPen(pen);
    /* horizontal line y=30 from x=10..49 ; Qt draws inclusive of both endpoints for a 1px cosmetic line */
    p.drawLine(10, 30, 49, 30);
    /* vertical line x=45 from y=5..54 */
    p.drawLine(45, 5, 45, 54);
    p.end();

    /* horizontal: sample interior points on the row */
    int hhits = 0;
    for (int x = 12; x <= 47; ++x) if (argb_close(img.pixel(x, 30), GREEN, 0)) ++hhits;
    g.check(hhits == 36, "drawLine: horizontal row coverage wrong");
    /* row above/below the horizontal line is background at a column away from the vertical line */
    g.check(argb_close(img.pixel(20, 29), BG, 0), "drawLine: pixel above h-line not bg");
    g.check(argb_close(img.pixel(20, 31), BG, 0), "drawLine: pixel below h-line not bg");
    /* vertical: sample interior points on the column */
    int vhits = 0;
    for (int y = 7; y <= 52; ++y) if (argb_close(img.pixel(45, y), GREEN, 0)) ++vhits;
    g.check(vhits == 46, "drawLine: vertical column coverage wrong");
    g.check(argb_close(img.pixel(44, 20), BG, 0), "drawLine: pixel left of v-line not bg");
}

/* ---- drawEllipse: filled circle analytic coverage ---- */
static void leg_ellipse(Gate &g) {
    const int W = 120, H = 120;
    QImage img = new_image(W, H, BG);
    QPainter p(&img);
    p.setRenderHint(QPainter::Antialiasing, false);
    p.setPen(Qt::NoPen);
    p.setBrush(QBrush(QColor(BLUE)));
    /* bounding box (10,10)-(110,110): diameter 100 -> center (60,60), radius 50 (Qt fills [x, x+w) span) */
    p.drawEllipse(10, 10, 100, 100);
    p.end();

    const double cx = 60.0, cy = 60.0, r = 50.0;
    /* center is fill */
    g.check(argb_close(img.pixel(60, 60), BLUE, 0), "ellipse: center not fill");
    /* well inside (r=30 from center) is fill */
    g.check(argb_close(img.pixel(90, 60), BLUE, 0), "ellipse: +30x not fill");
    g.check(argb_close(img.pixel(60, 90), BLUE, 0), "ellipse: +30y not fill");
    /* corner of bbox is outside the inscribed circle -> background */
    g.check(argb_close(img.pixel(12, 12), BG, 0),  "ellipse: bbox corner not bg");
    g.check(argb_close(img.pixel(107, 107), BG, 0),"ellipse: bbox far corner not bg");
    /* far outside the bbox entirely -> background */
    g.check(argb_close(img.pixel(2, 2), BG, 0),    "ellipse: outside-bbox not bg");
    /* analytic coverage: filled count within 6% of pi*r^2 (rasterization + Qt's half-open span) */
    int filled = count_color(img, 0, 0, W, H, BLUE, 0);
    double expect = M_PI * r * r;
    double frac = qAbs(filled - expect) / expect;
    if (frac >= 0.06)
        fprintf(stderr, "  ellipse: filled=%d expect~%.0f frac=%.3f\n", filled, expect, frac);
    g.check(frac < 0.06, "ellipse: filled area far from pi*r^2");
    /* every pixel strictly inside radius r-2 must be fill; outside r+2 (within bbox) must be bg */
    bool inside_ok = true, outside_ok = true;
    for (int y = 10; y < 110 && (inside_ok || outside_ok); ++y)
        for (int x = 10; x < 110; ++x) {
            double d = std::hypot(x + 0.5 - cx, y + 0.5 - cy);
            bool fill = argb_close(img.pixel(x, y), BLUE, 0);
            if (d < r - 2.0 && !fill) inside_ok = false;
            if (d > r + 2.0 && fill)  outside_ok = false;
        }
    g.check(inside_ok,  "ellipse: a pixel well inside r is not fill");
    g.check(outside_ok, "ellipse: a pixel well outside r is fill");
}

/* ---- Porter-Duff over: semi-transparent rect over an opaque one ---- */
static void leg_alpha(Gate &g) {
    QImage img = new_image(80, 80, BG);
    QPainter p(&img);
    p.setRenderHint(QPainter::Antialiasing, false);
    p.setCompositionMode(QPainter::CompositionMode_SourceOver);
    /* dst: opaque green rect covering the sample region */
    p.fillRect(10, 10, 60, 60, QColor(0x00, 0xFF, 0x00, 0xFF));
    /* src: red at alpha=128 over the green */
    p.fillRect(20, 20, 40, 40, QColor(0xFF, 0x00, 0x00, 128));
    p.end();

    /* closed form for the overlap region: src=(255,0,0,128) over dst=(0,255,0,255) */
    int oa = pd_over_alpha(128, 255);
    int orr = pd_over_chan(0xFF, 128, 0x00, 255);
    int ogg = pd_over_chan(0x00, 128, 0xFF, 255);
    int obb = pd_over_chan(0x00, 128, 0x00, 255);
    QRgb want = qRgba(orr, ogg, obb, oa);
    /* sample several points inside the overlap; tol 2 for rounding across arch */
    bool ok = true;
    for (int y = 25; y <= 55; y += 10)
        for (int x = 25; x <= 55; x += 10)
            if (!argb_close(img.pixel(x, y), want, 2)) { ok = false;
                fprintf(stderr, "  alpha: (%d,%d)=%08x want=%08x\n", x, y, img.pixel(x, y), want); }
    g.check(ok, "alpha: overlap not Porter-Duff over of red@128 on green");
    /* the green-only ring (dst with no src) stays pure opaque green */
    g.check(argb_close(img.pixel(12, 12), qRgba(0, 0xFF, 0, 0xFF), 1), "alpha: green-only ring disturbed");
    /* outside both rects stays background */
    g.check(argb_close(img.pixel(2, 2), BG, 0), "alpha: outside rects not bg");
}

/* ---- grab a real QLabel to a QImage: palette background + ink present ---- */
static void leg_label_grab(Gate &g) {
    QLabel label("HI");
    label.setFixedSize(120, 40);
    label.setAutoFillBackground(true);
    QPalette pal = label.palette();
    pal.setColor(QPalette::Window, QColor(0x30, 0x60, 0x90)); /* known bg */
    pal.setColor(QPalette::WindowText, QColor(0xFF, 0xFF, 0xFF));
    label.setPalette(pal);
    label.setAlignment(Qt::AlignCenter);

    QPixmap pm = label.grab();
    QImage img = pm.toImage().convertToFormat(QImage::Format_ARGB32);
    g.check(img.width() == 120 && img.height() == 40, "label: grabbed size wrong");

    QRgb winbg = qRgba(0x30, 0x60, 0x90, 0xFF);
    /* corners (padding away from centered text) carry the palette Window color */
    g.check(argb_close(img.pixel(2, 2), winbg, 4),                 "label: top-left not window bg");
    g.check(argb_close(img.pixel(img.width()-3, 2), winbg, 4),     "label: top-right not window bg");
    g.check(argb_close(img.pixel(2, img.height()-3), winbg, 4),    "label: bottom-left not window bg");
    /* text ink: some pixels differ from the window bg (the white glyphs) */
    int ink = count_non_bg(img, winbg, 24);
    g.check(ink > 20, "label: no glyph ink over background");
    /* the ink is concentrated near the center (aligned center), not in the far corners */
    int minx, miny, maxx, maxy;
    bool has = ink_bbox(img, winbg, 40, &minx, &miny, &maxx, &maxy);
    g.check(has, "label: ink bbox empty");
    if (has) {
        int cxmin = img.width() / 2 - 45, cxmax = img.width() / 2 + 45;
        g.check(minx >= 5 && maxx <= img.width() - 5, "label: ink touches horizontal edges");
        g.check((minx + maxx) / 2 >= cxmin && (minx + maxx) / 2 <= cxmax, "label: ink not centered");
    }
}

/* ---- text: one glyph, ink inside expected bbox, background elsewhere ---- */
static void leg_text(Gate &g) {
    const int W = 60, H = 60;
    QImage img = new_image(W, H, BG);
    QPainter p(&img);
    QFont f = p.font();
    f.setPixelSize(32);           /* fixed pixel size for determinism */
    p.setFont(f);
    p.setPen(QColor(0xFF, 0xFF, 0xFF));
    /* draw 'A' with baseline near (20,40); glyph ink should land roughly in x=[16,44] y=[14,42] */
    p.drawText(20, 40, QString("A"));
    p.end();

    int ink = count_non_bg(img, BG, 24);
    g.check(ink > 15, "text: glyph produced too little ink");
    int minx, miny, maxx, maxy;
    bool has = ink_bbox(img, BG, 24, &minx, &miny, &maxx, &maxy);
    g.check(has, "text: no ink at all");
    if (has) {
        /* ink stays within a generous but bounded box around the pen position (font-agnostic) */
        g.check(minx >= 10 && maxx <= 52, "text: ink outside expected x-range");
        g.check(miny >= 8  && maxy <= 46, "text: ink outside expected y-range");
        /* nothing drawn in the far top-left quadrant corner */
        g.check(argb_close(img.pixel(2, 2), BG, 0), "text: stray ink in top-left corner");
        g.check(argb_close(img.pixel(W-2, H-2), BG, 0), "text: stray ink in bottom-right corner");
    }
}

int main(int argc, char **argv) {
    QApplication app(argc, argv);
    Gate g("GUI_RENDER");
    leg_fillrect(g);
    leg_drawline(g);
    leg_ellipse(g);
    leg_alpha(g);
    leg_label_grab(g);
    leg_text(g);
    return g.finish();
}
