/* opencv_draw - 2D drawing primitives vs the analytic shape, per pixel (anti-aliasing OFF for exactness).
 *
 * rectangle (filled -> exact interior + clean exterior), line (axis-aligned exact row/col, diagonal on the
 * y=x cells), circle (filled -> center+radius samples fill, outside bbox clean, area within tol of pi r^2),
 * ellipse, polylines/fillPoly of a known triangle (exact fill count + a known interior/exterior pixel),
 * putText (ink lands in the expected bbox, corners clean). LINE_8 (no AA) so pixels are deterministic.
 */
#include "cv_common.h"
#include <opencv2/imgproc.hpp>

static int nz(const cv::Mat &m) { return cv::countNonZero(m); }

int main() {
    cv::setNumThreads(1);
    Gate g("OPENCV_DRAW");
    const int W = 64, H = 64;
    const cv::Scalar WHITE(255);

    /* rectangle filled: [x=10,y=8, w=20,h=12] -> interior [10,30)x[8,20) all 255, exterior 0.
       cv::rectangle with thickness=FILLED draws the inclusive rect [x1,x2]x[y1,y2]; use pt1(10,8)
       pt2(29,19) so the filled span is exactly 20x12 = 240 pixels. */
    cv::Mat r = cv::Mat::zeros(H, W, CV_8U);
    cv::rectangle(r, cv::Point(10, 8), cv::Point(29, 19), WHITE, cv::FILLED, cv::LINE_8);
    g.check(nz(r) == 20 * 12, "filled rectangle area != 240");
    bool rin = true, rout = true;
    for (int y = 0; y < H; y++) for (int x = 0; x < W; x++) {
        bool inside = (x >= 10 && x <= 29 && y >= 8 && y <= 19);
        if (inside) rin &= (r.at<uchar>(y, x) == 255);
        else        rout &= (r.at<uchar>(y, x) == 0);
    }
    g.check(rin, "filled rectangle interior not all 255");
    g.check(rout, "filled rectangle exterior not all 0");

    /* axis-aligned line: horizontal row y=30 from x=5..25 (thickness 1) -> exactly those 21 pixels set. */
    cv::Mat ln = cv::Mat::zeros(H, W, CV_8U);
    cv::line(ln, cv::Point(5, 30), cv::Point(25, 30), WHITE, 1, cv::LINE_8);
    int hits = 0;
    for (int x = 5; x <= 25; x++) if (ln.at<uchar>(30, x) == 255) hits++;
    g.check(hits == 21 && nz(ln) == 21, "horizontal line != 21 exact pixels on row 30");
    g.check(ln.at<uchar>(29, 15) == 0 && ln.at<uchar>(31, 15) == 0, "line leaked to adjacent rows");
    /* vertical line column x=40 from y=5..35 -> 31 pixels. */
    cv::Mat lv = cv::Mat::zeros(H, W, CV_8U);
    cv::line(lv, cv::Point(40, 5), cv::Point(40, 35), WHITE, 1, cv::LINE_8);
    g.check(nz(lv) == 31, "vertical line != 31 pixels");
    /* main-diagonal line from (0,0) to (20,20): the Bresenham diagonal hits every (i,i). */
    cv::Mat ld = cv::Mat::zeros(H, W, CV_8U);
    cv::line(ld, cv::Point(0, 0), cv::Point(20, 20), WHITE, 1, cv::LINE_8);
    bool diag_ok = true;
    for (int i = 0; i <= 20; i++) diag_ok &= (ld.at<uchar>(i, i) == 255);
    g.check(diag_ok, "diagonal line does not hit every (i,i)");

    /* filled circle center (32,32) r=12: center + points within r-1 are fill, points beyond r+1 are clean,
       area within 8% of pi r^2. */
    cv::Mat cc = cv::Mat::zeros(H, W, CV_8U);
    cv::circle(cc, cv::Point(32, 32), 12, WHITE, cv::FILLED, cv::LINE_8);
    g.check(cc.at<uchar>(32, 32) == 255, "circle center not filled");
    g.check(cc.at<uchar>(32, 32 + 10) == 255 && cc.at<uchar>(32 - 10, 32) == 255,
            "circle interior samples (r=10) not filled");
    g.check(cc.at<uchar>(32, 32 + 14) == 0 && cc.at<uchar>(32 + 14, 32) == 0,
            "circle exterior samples (r=14) not clean");
    double area = nz(cc), ideal = CV_PI * 12 * 12;
    g.check(std::fabs(area - ideal) / ideal < 0.08, "filled circle area not within 8% of pi r^2");
    /* every pixel with dist<=r-1.5 is fill and every pixel with dist>=r+1.5 is background. */
    bool sweep_ok = true;
    for (int y = 0; y < H; y++) for (int x = 0; x < W; x++) {
        double d = std::hypot(x - 32, y - 32);
        if (d <= 12 - 1.5 && cc.at<uchar>(y, x) != 255) sweep_ok = false;
        if (d >= 12 + 1.5 && cc.at<uchar>(y, x) != 0)   sweep_ok = false;
    }
    g.check(sweep_ok, "circle analytic coverage sweep failed");

    /* ellipse (full 0..360) center (32,32) axes (16,8): its bbox is [16,48]x[24,40]; center filled, a
       point outside the bbox (top-left corner) is clean. */
    cv::Mat el = cv::Mat::zeros(H, W, CV_8U);
    cv::ellipse(el, cv::Point(32, 32), cv::Size(16, 8), 0, 0, 360, WHITE, cv::FILLED, cv::LINE_8);
    g.check(el.at<uchar>(32, 32) == 255, "ellipse center not filled");
    g.check(el.at<uchar>(0, 0) == 0, "ellipse leaked to corner");
    /* semi-axis extents: (32+15,32) inside, (32,32+7) inside, (32+18,32) and (32,32+10) outside. */
    g.check(el.at<uchar>(32, 32 + 15) == 255 && el.at<uchar>(32 + 7, 32) == 255,
            "ellipse inside-axis samples not filled");
    g.check(el.at<uchar>(32, 32 + 18) == 0 && el.at<uchar>(32 + 10, 32) == 0,
            "ellipse outside-axis samples not clean");

    /* fillPoly / polylines of a known axis-aligned right triangle with vertices (10,10),(30,10),(10,30).
       The filled area is a discrete right triangle; a point well inside (12,12) is set, a point outside
       the hypotenuse (25,25) is clean. */
    cv::Mat tr = cv::Mat::zeros(H, W, CV_8U);
    std::vector<cv::Point> tri = {{10, 10}, {30, 10}, {10, 30}};
    std::vector<std::vector<cv::Point>> polys = {tri};
    cv::fillPoly(tr, polys, WHITE, cv::LINE_8);
    g.check(tr.at<uchar>(12, 12) == 255, "fillPoly interior (12,12) not set");
    g.check(tr.at<uchar>(25, 25) == 0, "fillPoly leaked past the hypotenuse at (25,25)");
    g.check(tr.at<uchar>(10, 10) == 255 && tr.at<uchar>(10, 29) == 255, "fillPoly corners not set");
    /* polylines (outline only) of the same triangle marks its top edge row 10 across x=10..30. */
    cv::Mat pl = cv::Mat::zeros(H, W, CV_8U);
    cv::polylines(pl, polys, true, WHITE, 1, cv::LINE_8);
    int top_edge = 0;
    for (int x = 10; x <= 30; x++) if (pl.at<uchar>(10, x) == 255) top_edge++;
    g.check(top_edge == 21, "polylines top edge != 21 pixels");

    /* putText: ink lands inside the text bbox and the far corners stay clean (font-agnostic). */
    cv::Mat tx = cv::Mat::zeros(32, 96, CV_8U);
    cv::putText(tx, "Hi", cv::Point(4, 22), cv::FONT_HERSHEY_SIMPLEX, 0.8, WHITE, 1, cv::LINE_8);
    g.check(nz(tx) > 0, "putText produced no ink");
    /* ink bbox stays left/upper region; the bottom-right corner is untouched. */
    g.check(tx.at<uchar>(31, 95) == 0 && tx.at<uchar>(0, 95) == 0, "putText leaked to far corners");
    int left_ink = nz(tx(cv::Rect(0, 0, 48, 32)));
    g.check(left_ink == nz(tx), "putText ink not confined to the left half where it was drawn");

    return g.finish();
}
