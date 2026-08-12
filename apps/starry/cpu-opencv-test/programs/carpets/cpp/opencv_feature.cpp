/* opencv_feature - feature/edge detectors on KNOWN geometry vs the known answer.
 *
 * Canny on a vertical step edge -> edges localized at the known transition column; cornerHarris /
 * goodFeaturesToTrack on a checkerboard -> corner responses at the known grid intersections; HoughLinesP on
 * a single drawn horizontal line -> a near-horizontal segment at the known row. Tolerances only where the
 * detector legitimately has sub-pixel/threshold slack; the LOCATION is asserted. No RNG (fixed image).
 */
#include "cv_common.h"
#include <opencv2/imgproc.hpp>
#include <opencv2/features2d.hpp>
#include <vector>
#include <cmath>

int main() {
    cv::setNumThreads(1);
    Gate g("OPENCV_FEATURE");

    /* ---- Canny on a vertical step edge at column 20 ---- */
    cv::Mat step = cv::Mat::zeros(40, 40, CV_8U);
    step(cv::Rect(20, 0, 20, 40)).setTo(255);   /* left half 0, right half 255 */
    cv::Mat edges; cv::Canny(step, edges, 50, 150);
    /* the only edge is the vertical transition: every edge pixel sits in columns 19..20, and every row in
       the interior has an edge there. */
    bool edge_col_ok = true; int edge_rows = 0;
    for (int y = 1; y < 39; y++) {
        bool has = false;
        for (int x = 0; x < 40; x++) if (edges.at<uchar>(y, x)) {
            has = true;
            if (x < 18 || x > 21) edge_col_ok = false;   /* edge must be at the step */
        }
        if (has) edge_rows++;
    }
    g.check(edge_col_ok, "Canny edge not localized at the step column (18..21)");
    g.check(edge_rows >= 36, "Canny did not find the edge on (nearly) every interior row");
    /* no edges in the flat interiors far from the step. */
    g.check(edges.at<uchar>(20, 5) == 0 && edges.at<uchar>(20, 35) == 0, "Canny fired in a flat region");

    /* ---- cornerHarris on a checkerboard: strong response at the interior grid intersection ---- */
    /* 4x4 board of 10px squares (40x40); interior corners sit at multiples of 10: (10,10),(10,20)... */
    cv::Mat board = cv::Mat::zeros(40, 40, CV_8U);
    for (int by = 0; by < 4; by++) for (int bx = 0; bx < 4; bx++)
        if ((bx + by) & 1) board(cv::Rect(bx * 10, by * 10, 10, 10)).setTo(255);
    cv::Mat harris; cv::cornerHarris(board, harris, 2, 3, 0.04);
    double hmin, hmax; cv::minMaxLoc(harris, &hmin, &hmax);
    g.check(hmax > 0, "cornerHarris produced no positive response");
    /* the response at a true interior intersection (20,20) is a large fraction of the max; a point in the
       middle of a flat square (5,5) is essentially zero. */
    float resp_corner = harris.at<float>(20, 20);
    float resp_flat = std::fabs(harris.at<float>(5, 5));
    g.check(resp_corner > 0.2f * (float)hmax, "Harris response weak at a known intersection (20,20)");
    g.check(resp_flat < 0.05f * (float)hmax, "Harris response not ~0 inside a flat square (5,5)");

    /* ---- goodFeaturesToTrack on the same board: detected corners snap to grid intersections ---- */
    std::vector<cv::Point2f> corners;
    cv::goodFeaturesToTrack(board, corners, 20, 0.1, 5);
    g.check(!corners.empty(), "goodFeaturesToTrack found no corners on a checkerboard");
    /* every detected corner lies within 2px of a multiple-of-10 grid intersection. */
    bool on_grid = true;
    for (auto &c : corners) {
        double rx = std::fabs(c.x - std::round(c.x / 10.0) * 10.0);
        double ry = std::fabs(c.y - std::round(c.y / 10.0) * 10.0);
        if (rx > 2.5 || ry > 2.5) on_grid = false;
    }
    g.check(on_grid, "a detected corner is not near a grid intersection");

    /* ---- HoughLinesP on a single horizontal line at row 25 ---- */
    cv::Mat lineimg = cv::Mat::zeros(60, 60, CV_8U);
    cv::line(lineimg, cv::Point(5, 25), cv::Point(54, 25), 255, 1, cv::LINE_8);
    std::vector<cv::Vec4i> lines;
    cv::HoughLinesP(lineimg, lines, 1, CV_PI / 180.0, 30, 30, 5);
    g.check(!lines.empty(), "HoughLinesP found no line");
    /* at least one detected segment is horizontal (|dy|<=1) and sits at row ~25. */
    bool found_h = false;
    for (auto &L : lines) {
        int x1 = L[0], y1 = L[1], x2 = L[2], y2 = L[3];
        if (std::abs(y1 - y2) <= 1 && std::abs(y1 - 25) <= 1 && std::abs(x2 - x1) >= 30) found_h = true;
    }
    g.check(found_h, "HoughLinesP did not recover the horizontal line at row 25");

    return g.finish();
}
