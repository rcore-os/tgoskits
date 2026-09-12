/* opencv_morph - thresholding & morphology vs closed form.
 *
 * threshold BINARY on a known ramp (exact split point), Otsu on a known bimodal histogram (known optimal
 * threshold), erode/dilate/open/close of a known binary pattern vs the closed-form structuring-element
 * result, connectedComponents count on a known multi-blob image. No RNG.
 */
#include "cv_common.h"
#include <opencv2/imgproc.hpp>

int main() {
    cv::setNumThreads(1);
    Gate g("OPENCV_MORPH");

    /* threshold BINARY at t=100: value>100 -> 255 else 0. Known ramp 0,50,100,150,200. */
    cv::Mat ramp = (cv::Mat_<uchar>(1, 5) << 0, 50, 100, 150, 200);
    cv::Mat th; cv::threshold(ramp, th, 100, 255, cv::THRESH_BINARY);
    /* OpenCV BINARY is strictly ">" thresh: 100 stays 0, 150/200 -> 255. */
    g.check(th.at<uchar>(0, 0) == 0 && th.at<uchar>(0, 1) == 0 && th.at<uchar>(0, 2) == 0 &&
            th.at<uchar>(0, 3) == 255 && th.at<uchar>(0, 4) == 255, "THRESH_BINARY split wrong");
    cv::Mat thi; cv::threshold(ramp, thi, 100, 255, cv::THRESH_BINARY_INV);
    g.check(thi.at<uchar>(0, 2) == 255 && thi.at<uchar>(0, 3) == 0, "THRESH_BINARY_INV split wrong");
    cv::Mat tt; cv::threshold(ramp, tt, 100, 0, cv::THRESH_TRUNC);
    g.check(tt.at<uchar>(0, 3) == 100 && tt.at<uchar>(0, 1) == 50, "THRESH_TRUNC wrong");

    /* Otsu on a perfectly bimodal image: half pixels at 20, half at 200. The optimal threshold sits
       between the two clusters; OpenCV returns it. Verify the returned threshold separates the modes and
       the binarization puts every 200 -> 255 and every 20 -> 0. */
    cv::Mat bim(1, 8, CV_8U);
    for (int i = 0; i < 4; i++) bim.at<uchar>(0, i) = 20;
    for (int i = 4; i < 8; i++) bim.at<uchar>(0, i) = 200;
    cv::Mat ot; double otsu_t = cv::threshold(bim, ot, 0, 255, cv::THRESH_BINARY | cv::THRESH_OTSU);
    /* with two discrete levels (20,200), between-class variance peaks at t=20: everything >20 (the 200s)
       goes foreground, the 20s go background - the closed-form optimal split. */
    g.check(otsu_t >= 20 && otsu_t < 200, "Otsu threshold not in the mode-separating range");
    bool otsu_ok = true;
    for (int i = 0; i < 4; i++) otsu_ok &= (ot.at<uchar>(0, i) == 0);
    for (int i = 4; i < 8; i++) otsu_ok &= (ot.at<uchar>(0, i) == 255);
    g.check(otsu_ok, "Otsu binarization did not separate the modes");

    /* erode/dilate with a 3x3 rectangular SE on a single foreground pixel:
       dilate -> a 3x3 block of 255; erode of that block -> back to the single pixel. */
    cv::Mat dot = cv::Mat::zeros(7, 7, CV_8U);
    dot.at<uchar>(3, 3) = 255;
    cv::Mat se = cv::getStructuringElement(cv::MORPH_RECT, cv::Size(3, 3));
    cv::Mat dil; cv::dilate(dot, dil, se);
    int dcount = 0;
    for (int y = 0; y < 7; y++) for (int x = 0; x < 7; x++) if (dil.at<uchar>(y, x)) dcount++;
    g.check(dcount == 9, "dilate of a dot != 3x3 block (9 px)");
    bool block_ok = true;
    for (int y = 2; y <= 4; y++) for (int x = 2; x <= 4; x++) block_ok &= (dil.at<uchar>(y, x) == 255);
    g.check(block_ok, "dilate block not centered at (3,3)");
    cv::Mat ero; cv::erode(dil, ero, se);
    int ecount = 0;
    for (int y = 0; y < 7; y++) for (int x = 0; x < 7; x++) if (ero.at<uchar>(y, x)) ecount++;
    g.check(ecount == 1 && ero.at<uchar>(3, 3) == 255, "erode(dilate(dot)) != original dot");

    /* opening removes a lone speck (erode then dilate): a single pixel disappears entirely. */
    cv::Mat speck = cv::Mat::zeros(7, 7, CV_8U); speck.at<uchar>(1, 1) = 255;
    cv::Mat opened; cv::morphologyEx(speck, opened, cv::MORPH_OPEN, se);
    g.check(cv::countNonZero(opened) == 0, "opening did not remove a lone speck");

    /* closing fills a lone hole in a solid block (dilate then erode). */
    cv::Mat solid(7, 7, CV_8U, cv::Scalar(255));
    solid.at<uchar>(3, 3) = 0;   /* one hole */
    cv::Mat closed; cv::morphologyEx(solid, closed, cv::MORPH_CLOSE, se);
    g.check(closed.at<uchar>(3, 3) == 255, "closing did not fill the hole");

    /* connectedComponents: three separated 1-pixel blobs -> 3 labels + background = 4 total. */
    cv::Mat blobs = cv::Mat::zeros(9, 9, CV_8U);
    blobs.at<uchar>(1, 1) = 255; blobs.at<uchar>(1, 7) = 255; blobs.at<uchar>(7, 4) = 255;
    cv::Mat labels; int n = cv::connectedComponents(blobs, labels, 8, CV_32S);
    g.check(n == 4, "connectedComponents count != 4 (bg + 3 blobs)");
    /* the three blob pixels carry distinct nonzero labels; background is label 0. */
    int l1 = labels.at<int>(1, 1), l2 = labels.at<int>(1, 7), l3 = labels.at<int>(7, 4);
    g.check(l1 && l2 && l3 && l1 != l2 && l2 != l3 && l1 != l3 && labels.at<int>(0, 0) == 0,
            "blob labels not distinct/background not 0");

    return g.finish();
}
