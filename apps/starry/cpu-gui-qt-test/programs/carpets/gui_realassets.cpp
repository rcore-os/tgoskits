/* gui_realassets.cpp - load a real font file from ASSET_DIR into Qt and render a label with it.
 *
 * This is the optional real-asset leg: it honest-skips (still prints its OK marker with the skip checks) when
 * no font is present under ASSET_DIR / FONT_DIR, so the synthetic legs (cells 1-3) always gate on their own.
 * When a font IS present:
 *   - QFontDatabase::addApplicationFont loads it; assert it registered a family.
 *   - render a known string with that family at a fixed pixel size into an ARGB32 image;
 *   - assert ink appears (non-empty coverage) and lands inside a bounded box, and the background outside the
 *     text run is untouched. Font-agnostic (bbox + coverage, never glyph-exact pixels).
 *
 * ASSET_DIR default matches the prebuild staging (/opt/cpu-gui-qt-test/assets). A DejaVu font is staged by
 * prebuild when font-dejavu is available; absent that, the leg records an honest skip and still passes.
 */
#include "gui_common.h"
#include <QApplication>
#include <QFontDatabase>
#include <QPainter>
#include <QImage>
#include <QString>
#include <QDir>
#include <cstdlib>

static QString asset_dir() {
    const char *d = getenv("ASSET_DIR");
    if (d && *d) return QString::fromUtf8(d);
    d = getenv("FONT_DIR");
    if (d && *d) return QString::fromUtf8(d);
    return QStringLiteral("/opt/cpu-gui-qt-test/assets");
}

/* find the first .ttf/.otf under the asset dir; empty string if none */
static QString find_font(const QString &dir) {
    QDir d(dir);
    if (!d.exists()) return QString();
    const QStringList filters{"*.ttf", "*.otf", "*.ttc"};
    const QStringList hits = d.entryList(filters, QDir::Files, QDir::Name);
    if (hits.isEmpty()) return QString();
    return d.absoluteFilePath(hits.first());
}

int main(int argc, char **argv) {
    QApplication app(argc, argv);
    Gate g("GUI_REALASSETS");

    QString dir = asset_dir();
    QString font = find_font(dir);

    if (font.isEmpty()) {
        /* honest skip: no external font present -> the synthetic legs already gate. Record the skip as a
         * passing check so the three-gate marker still fires (total>0, fail==0). */
        fprintf(stderr, "  gui_realassets: no font under %s - honest skip\n", qUtf8Printable(dir));
        g.check(true, "realassets honest-skip (no font asset)");
        return g.finish();
    }

    int id = QFontDatabase::addApplicationFont(font);
    g.check(id >= 0, "realassets: addApplicationFont failed");
    QStringList fams = QFontDatabase::applicationFontFamilies(id);
    g.check(!fams.isEmpty(), "realassets: loaded font registered no family");
    if (fams.isEmpty()) return g.finish();

    const QRgb BG = qRgba(0x10, 0x10, 0x10, 0xFF);
    const int W = 200, H = 60;
    QImage img = new_image(W, H, BG);
    {
        QPainter p(&img);
        QFont f(fams.first());
        f.setPixelSize(28);
        p.setFont(f);
        p.setPen(QColor(0xFF, 0xFF, 0xFF));
        p.drawText(12, 40, QStringLiteral("Starry"));
    }

    int ink = count_non_bg(img, BG, 24);
    g.check(ink > 60, "realassets: rendered string produced too little ink");
    int minx, miny, maxx, maxy;
    bool has = ink_bbox(img, BG, 24, &minx, &miny, &maxx, &maxy);
    g.check(has, "realassets: no ink from rendered string");
    if (has) {
        /* the text run starts near x=12 and stays within the image with a right margin */
        g.check(minx >= 6 && minx <= 30, "realassets: ink does not start near the pen x");
        g.check(maxx <= W - 4, "realassets: ink overruns right edge");
        g.check(miny >= 8 && maxy <= H - 4, "realassets: ink outside vertical band");
        /* background clearly outside the text run (bottom-right corner) is untouched */
        g.check(argb_close(img.pixel(W - 2, H - 2), BG, 0), "realassets: stray ink in far corner");
    }
    return g.finish();
}
