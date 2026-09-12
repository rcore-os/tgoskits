/* opencv_geometry - geometric transforms vs closed form, at known points.
 *
 * resize INTER_NEAREST (exact source pixel), INTER_LINEAR (bilinear closed form at known coords), flip,
 * transpose, warpAffine translation (exact pixel shift), getRotationMatrix2D matrix values, warpAffine
 * 90-degree rotation (exact pixel mapping). No RNG.
 */
#include "cv_common.h"
#include <opencv2/imgproc.hpp>

int main() {
    cv::setNumThreads(1);
    Gate g("OPENCV_GEOMETRY");

    /* 2x2 source with distinct values so mappings are unambiguous. */
    cv::Mat src = (cv::Mat_<uchar>(2, 2) << 10, 20, 30, 40);

    /* resize x2 INTER_NEAREST: each source pixel replicated into a 2x2 block. */
    cv::Mat up; cv::resize(src, up, cv::Size(4, 4), 0, 0, cv::INTER_NEAREST);
    bool nn_ok = true;
    for (int y = 0; y < 4; y++) for (int x = 0; x < 4; x++)
        nn_ok &= (up.at<uchar>(y, x) == src.at<uchar>(y / 2, x / 2));
    g.check(nn_ok, "resize NEAREST x2 != block replication");

    /* resize INTER_LINEAR: bilinear at a known point. Scale a 2x2 [[0,10],[20,30]] to 4x4; OpenCV maps
       dst (x,y) to src ((x+0.5)*sx-0.5, ...) with sx=2/4=0.5. dst(1,1) -> src(0.25,0.25):
       f = (1-0.25)(1-0.25)*0 + 0.25(1-0.25)*10 + (1-0.25)0.25*20 + 0.25*0.25*30
         = 1.875 + 3.75 + 1.875 = 7.5 -> rounds to 8. */
    cv::Mat lin = (cv::Mat_<uchar>(2, 2) << 0, 10, 20, 30);
    cv::Mat linup; cv::resize(lin, linup, cv::Size(4, 4), 0, 0, cv::INTER_LINEAR);
    g.check(close_i(linup.at<uchar>(1, 1), 8, 1), "bilinear dst(1,1) != ~7.5");
    /* corner dst(0,0) -> src(-0.25,-0.25) clamped to (0,0) => value 0 exactly. */
    g.check(linup.at<uchar>(0, 0) == 0, "bilinear corner(0,0) != 0");

    /* flip: horizontal (code 1) swaps columns; vertical (code 0) swaps rows. */
    cv::Mat fh; cv::flip(src, fh, 1);
    g.check(fh.at<uchar>(0, 0) == 20 && fh.at<uchar>(0, 1) == 10 &&
            fh.at<uchar>(1, 0) == 40 && fh.at<uchar>(1, 1) == 30, "flip horizontal mismatch");
    cv::Mat fv; cv::flip(src, fv, 0);
    g.check(fv.at<uchar>(0, 0) == 30 && fv.at<uchar>(1, 0) == 10, "flip vertical mismatch");

    /* transpose: dst(j,i) == src(i,j). */
    cv::Mat tp; cv::transpose(src, tp);
    g.check(tp.at<uchar>(0, 1) == src.at<uchar>(1, 0) && tp.at<uchar>(1, 0) == src.at<uchar>(0, 1),
            "transpose mismatch");

    /* warpAffine pure translation by (+1,+1): dst(y,x) == src(y-1,x-1); a marker pixel moves exactly. */
    cv::Mat marker = cv::Mat::zeros(5, 5, CV_8U);
    marker.at<uchar>(1, 1) = 200;
    cv::Mat Tm = (cv::Mat_<double>(2, 3) << 1, 0, 1, 0, 1, 1);   /* shift +1 x, +1 y */
    cv::Mat shifted; cv::warpAffine(marker, shifted, Tm, marker.size(), cv::INTER_NEAREST);
    g.check(shifted.at<uchar>(2, 2) == 200, "warpAffine translation did not move marker to (2,2)");
    g.check(shifted.at<uchar>(1, 1) == 0, "warpAffine left a ghost at old position");

    /* getRotationMatrix2D(center, 90deg, 1.0): [[cos,sin,...],[-sin,cos,...]] with cos0 sin1. */
    cv::Point2f ctr(2, 2);
    cv::Mat R = cv::getRotationMatrix2D(ctr, 90.0, 1.0);
    g.check(close_d(R.at<double>(0, 0), 0.0, 1e-9) && close_d(R.at<double>(0, 1), 1.0, 1e-9) &&
            close_d(R.at<double>(1, 0), -1.0, 1e-9) && close_d(R.at<double>(1, 1), 0.0, 1e-9),
            "getRotationMatrix2D(90) cos/sin block wrong");
    /* the affine maps center to itself: R*[cx,cy,1]^T == [cx,cy]. */
    double mx = R.at<double>(0, 0) * 2 + R.at<double>(0, 1) * 2 + R.at<double>(0, 2);
    double my = R.at<double>(1, 0) * 2 + R.at<double>(1, 1) * 2 + R.at<double>(1, 2);
    g.check(close_d(mx, 2, 1e-9) && close_d(my, 2, 1e-9), "rotation does not fix its center");

    /* warpAffine 90deg rotation about center: a marker at (1,2) maps to the known rotated location. For a
       5x5 image with center (2,2) and +90deg (OpenCV CCW in image coords via the matrix above),
       dst = R * [x,y,1]. Marker src pixel appears where R maps it. Compute the exact destination. */
    cv::Mat rimg = cv::Mat::zeros(5, 5, CV_8U);
    rimg.at<uchar>(2, 1) = 150;   /* (x=1,y=2) */
    cv::Mat rot; cv::warpAffine(rimg, rot, R, rimg.size(), cv::INTER_NEAREST);
    int dx = (int)std::lround(R.at<double>(0, 0) * 1 + R.at<double>(0, 1) * 2 + R.at<double>(0, 2));
    int dy = (int)std::lround(R.at<double>(1, 0) * 1 + R.at<double>(1, 1) * 2 + R.at<double>(1, 2));
    g.check(dx >= 0 && dx < 5 && dy >= 0 && dy < 5 && rot.at<uchar>(dy, dx) == 150,
            "warpAffine 90deg did not land marker at closed-form (dx,dy)");

    /* getAffineTransform from 3 point pairs of a pure +1/+1 translation reproduces the shift matrix. */
    cv::Point2f s3[3] = {{0, 0}, {1, 0}, {0, 1}}, d3[3] = {{1, 1}, {2, 1}, {1, 2}};
    cv::Mat AT = cv::getAffineTransform(s3, d3);
    g.check(close_d(AT.at<double>(0, 0), 1, 1e-6) && close_d(AT.at<double>(0, 2), 1, 1e-6) &&
            close_d(AT.at<double>(1, 1), 1, 1e-6) && close_d(AT.at<double>(1, 2), 1, 1e-6),
            "getAffineTransform of a +1/+1 shift wrong");

    return g.finish();
}
