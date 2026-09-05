/* gui_layout.cpp - deterministic geometry: assert each child's computed geometry() == closed-form layout math.
 *
 * Layouts are given fixed margins and spacing and fixed-size children, the host is sized to the layout's
 * own sizeHint, then shown and the event loop flushed so Qt actually performs the layout, and each child's
 * geometry() is compared to the exact arithmetic:
 *   - QVBoxLayout: child i at y = margin + i*(childH + spacing), x = margin, over the fixed width.
 *   - QHBoxLayout: child i at x = margin + i*(childW + spacing).
 *   - QGridLayout: cell (r,c) at the row/col offsets from fixed-size, spacing and margins.
 *   - resize the parent -> stretchable children re-layout to the new closed-form width/height.
 *   - sizeHint / minimumSizeHint composition of a nested layout equals the summed child extents + spacing.
 *
 * No pixels needed: layout math is exact integer arithmetic, identical across arch. The offscreen QPA does
 * not propagateSizeHints (it prints a benign notice), which is irrelevant here: we drive the geometry via
 * explicit resize() + show() + processEvents() and read the realized child geometry() back.
 */
#include "gui_common.h"
#include <QApplication>
#include <QWidget>
#include <QVBoxLayout>
#include <QHBoxLayout>
#include <QGridLayout>
#include <QLabel>

/* a fixed-size child so the layout has no freedom to resize it */
static QLabel *fixed_child(int w, int h) {
    QLabel *l = new QLabel;
    l->setFixedSize(w, h);
    return l;
}

/* realize the host at a definite size and flush the layout pass */
static void realize(QWidget &host) {
    host.show();
    QApplication::processEvents();
}

/* ---- QVBoxLayout: children stack vertically at known offsets ---- */
static void leg_vbox(Gate &g) {
    QWidget host;
    QVBoxLayout *v = new QVBoxLayout(&host);
    const int M = 9, S = 7, CW = 120, CH = 30;
    v->setContentsMargins(M, M, M, M);
    v->setSpacing(S);
    QLabel *c[3];
    for (int i = 0; i < 3; ++i) { c[i] = fixed_child(CW, CH); v->addWidget(c[i]); }
    int wantW = M + CW + M;
    int wantH = M + 3 * CH + 2 * S + M;
    host.resize(wantW, wantH);
    realize(host);

    for (int i = 0; i < 3; ++i) {
        int ey = M + i * (CH + S);
        QRect r = c[i]->geometry();
        char msg[96]; snprintf(msg, sizeof msg, "vbox child %d geometry != closed form", i);
        bool ok = r.x() == M && r.y() == ey && r.width() == CW && r.height() == CH;
        if (!ok) fprintf(stderr, "  vbox[%d] got (%d,%d,%dx%d) want (%d,%d,%dx%d)\n",
                         i, r.x(), r.y(), r.width(), r.height(), M, ey, CW, CH);
        g.check(ok, msg);
    }
    /* the layout's own sizeHint = margins + 3 children + 2 gaps */
    g.check(host.sizeHint().height() == wantH, "vbox: sizeHint height != summed child extents+spacing+margins");
}

/* ---- QHBoxLayout: children stack horizontally at known offsets ---- */
static void leg_hbox(Gate &g) {
    QWidget host;
    QHBoxLayout *h = new QHBoxLayout(&host);
    const int M = 5, S = 11, CW = 40, CH = 50;
    h->setContentsMargins(M, M, M, M);
    h->setSpacing(S);
    QLabel *c[4];
    for (int i = 0; i < 4; ++i) { c[i] = fixed_child(CW, CH); h->addWidget(c[i]); }
    int wantW = M + 4 * CW + 3 * S + M;
    int wantH = M + CH + M;
    host.resize(wantW, wantH);
    realize(host);

    for (int i = 0; i < 4; ++i) {
        int ex = M + i * (CW + S);
        QRect r = c[i]->geometry();
        char msg[96]; snprintf(msg, sizeof msg, "hbox child %d geometry != closed form", i);
        bool ok = r.x() == ex && r.y() == M && r.width() == CW && r.height() == CH;
        if (!ok) fprintf(stderr, "  hbox[%d] got (%d,%d,%dx%d) want (%d,%d,%dx%d)\n",
                         i, r.x(), r.y(), r.width(), r.height(), ex, M, CW, CH);
        g.check(ok, msg);
    }
    g.check(host.sizeHint().width() == wantW, "hbox: sizeHint width != summed child extents+spacing+margins");
}

/* ---- QGridLayout: 2x2 fixed cells at row/col offsets ---- */
static void leg_grid(Gate &g) {
    QWidget host;
    QGridLayout *gl = new QGridLayout(&host);
    const int M = 8, HS = 6, VS = 10, CW = 60, CH = 40;
    gl->setContentsMargins(M, M, M, M);
    gl->setHorizontalSpacing(HS);
    gl->setVerticalSpacing(VS);
    QLabel *c[2][2];
    for (int r = 0; r < 2; ++r)
        for (int col = 0; col < 2; ++col) { c[r][col] = fixed_child(CW, CH); gl->addWidget(c[r][col], r, col); }
    int wantW = M + 2 * CW + HS + M;
    int wantH = M + 2 * CH + VS + M;
    host.resize(wantW, wantH);
    realize(host);

    for (int r = 0; r < 2; ++r)
        for (int col = 0; col < 2; ++col) {
            int ex = M + col * (CW + HS);
            int ey = M + r * (CH + VS);
            QRect rc = c[r][col]->geometry();
            char msg[96]; snprintf(msg, sizeof msg, "grid cell (%d,%d) geometry != closed form", r, col);
            bool ok = rc.x() == ex && rc.y() == ey && rc.width() == CW && rc.height() == CH;
            if (!ok) fprintf(stderr, "  grid[%d][%d] got (%d,%d,%dx%d) want (%d,%d,%dx%d)\n",
                             r, col, rc.x(), rc.y(), rc.width(), rc.height(), ex, ey, CW, CH);
            g.check(ok, msg);
        }
    g.check(host.sizeHint() == QSize(wantW, wantH), "grid: sizeHint != closed form");
}

/* ---- resize: stretchable children re-layout to the new closed-form size ---- */
static void leg_resize_stretch(Gate &g) {
    QWidget host;
    QVBoxLayout *v = new QVBoxLayout(&host);
    const int M = 10, S = 8;
    v->setContentsMargins(M, M, M, M);
    v->setSpacing(S);
    /* two expanding labels split the vertical space equally */
    QLabel *a = new QLabel, *b = new QLabel;
    a->setSizePolicy(QSizePolicy::Expanding, QSizePolicy::Expanding);
    b->setSizePolicy(QSizePolicy::Expanding, QSizePolicy::Expanding);
    a->setMinimumSize(0, 0); b->setMinimumSize(0, 0);
    v->addWidget(a);
    v->addWidget(b);

    host.resize(200, 300);
    realize(host);
    /* usable height = 300 - 2*M - S ; split in two ; width fills the content region */
    int usable = 300 - 2 * M - S;
    int half = usable / 2;
    int fillW = 200 - 2 * M;
    g.check(a->geometry().x() == M && a->geometry().y() == M, "resize: top child origin wrong");
    g.check(a->width() == fillW, "resize: top child width != content width");
    g.check(qAbs(a->height() - half) <= 1, "resize: top child height != half usable");
    g.check(b->geometry().y() == M + a->height() + S, "resize: bottom child y != top+height+spacing");
    g.check(qAbs((a->height() + b->height()) - usable) <= 1, "resize: children do not fill usable height");

    /* resize larger; children re-layout to the new closed form */
    host.resize(200, 500);
    QApplication::processEvents();
    int usable2 = 500 - 2 * M - S;
    g.check(qAbs((a->height() + b->height()) - usable2) <= 1, "resize(500): children do not fill new usable height");
    g.check(b->geometry().y() == M + a->height() + S, "resize(500): bottom child re-position wrong");
}

/* ---- sizeHint / minimumSizeHint composition of a nested layout ---- */
static void leg_sizehint(Gate &g) {
    QWidget host;
    QHBoxLayout *h = new QHBoxLayout(&host);
    const int M = 4, S = 6, CW = 30, CH = 20;
    h->setContentsMargins(M, M, M, M);
    h->setSpacing(S);
    for (int i = 0; i < 3; ++i) h->addWidget(fixed_child(CW, CH));
    QSize hint = host.sizeHint();
    int wantW = M + 3 * CW + 2 * S + M;
    int wantH = M + CH + M;
    g.check(hint.width() == wantW, "sizehint: width != composed extents");
    g.check(hint.height() == wantH, "sizehint: height != composed extents");
    QSize mn = host.minimumSizeHint();
    g.check(mn.width() == wantW && mn.height() == wantH, "sizehint: minimumSizeHint != fixed-child composition");
}

int main(int argc, char **argv) {
    QApplication app(argc, argv);
    Gate g("GUI_LAYOUT");
    leg_vbox(g);
    leg_hbox(g);
    leg_grid(g);
    leg_resize_stretch(g);
    leg_sizehint(g);
    return g.finish();
}
