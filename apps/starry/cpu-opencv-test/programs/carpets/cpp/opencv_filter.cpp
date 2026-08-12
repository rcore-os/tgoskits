/* opencv_filter - convolution/filtering vs closed form, per pixel.
 *
 * GaussianBlur of a single-pixel impulse -> the output equals the normalized separable Gaussian kernel
 * (assert against cv::getGaussianKernel's outer product, and pin the exact center/neighbor weights);
 * Sobel of a known linear ramp -> a constant derivative; boxFilter of a constant -> the same constant;
 * medianBlur removes a lone outlier while leaving a constant field untouched. No RNG.
 */
#include "cv_common.h"
#include <opencv2/imgproc.hpp>

int main() {
    cv::setNumThreads(1);
    Gate g("OPENCV_FILTER");

    const int N = 9, c = N / 2;   /* impulse at the center of a 9x9 field */

    /* ---- GaussianBlur impulse -> normalized Gaussian kernel ---- */
    cv::Mat impulse = cv::Mat::zeros(N, N, CV_32F);
    impulse.at<float>(c, c) = 1.0f;
    int ks = 5; double sigma = 1.0;
    cv::Mat blur;
    cv::GaussianBlur(impulse, blur, cv::Size(ks, ks), sigma, sigma, cv::BORDER_CONSTANT);

    /* closed form: separable kernel k = getGaussianKernel(ks,sigma); 2D weight = k[i]*k[j]. The blurred
       impulse at (c+dy, c+dx) must equal that product exactly (BORDER_CONSTANT, impulse fully interior). */
    cv::Mat k1 = cv::getGaussianKernel(ks, sigma, CV_32F);   /* ks x 1 */
    bool gk_ok = true; double ksum = 0;
    for (int dy = -ks / 2; dy <= ks / 2; dy++)
        for (int dx = -ks / 2; dx <= ks / 2; dx++) {
            float want = k1.at<float>(dy + ks / 2) * k1.at<float>(dx + ks / 2);
            float got = blur.at<float>(c + dy, c + dx);
            gk_ok &= close_d(got, want, 1e-6);
            ksum += got;
        }
    g.check(gk_ok, "GaussianBlur(impulse) != outer(k,k) closed form");
    g.check(close_d(ksum, 1.0, 1e-5), "Gaussian kernel does not sum to 1");
    /* pin the exact getGaussianKernel(5,1.0) values (symmetric, host-calibrated):
       [0.054489, 0.244201, 0.40262, 0.244201, 0.054489]. Center 2D weight = 0.40262^2 = 0.162103. */
    g.check(close_d(k1.at<float>(0), 0.054489f, 1e-4), "outer Gaussian tap != 0.054489");
    g.check(close_d(k1.at<float>(1), 0.244201f, 1e-4), "mid Gaussian tap != 0.244201");
    g.check(close_d(k1.at<float>(2), 0.40262f, 1e-4), "center Gaussian tap != 0.40262");
    g.check(close_d(blur.at<float>(c, c), 0.40262f * 0.40262f, 1e-4), "center pixel != w0^2");
    /* far pixels (outside the kernel support) stay exactly zero. */
    g.check(blur.at<float>(0, 0) == 0.0f && blur.at<float>(N - 1, N - 1) == 0.0f,
            "Gaussian leaked outside kernel support");

    /* ---- Sobel of a linear ramp -> constant derivative ---- */
    /* image f(x,y) = 10*x (ramp along x). d/dx = 10; a 3x3 Sobel x scales the gradient by 8 (sum of the
       positive weights on one side = 1+2+1 = 4 per column pair => factor 8 for a unit-slope ramp; for
       slope 10 the response is 8*10 = 80 in interior pixels). */
    cv::Mat ramp(7, 7, CV_32F);
    for (int y = 0; y < 7; y++) for (int x = 0; x < 7; x++) ramp.at<float>(y, x) = 10.0f * x;
    cv::Mat sx;
    cv::Sobel(ramp, sx, CV_32F, 1, 0, 3, 1, 0, cv::BORDER_REPLICATE);
    bool sob_ok = true;
    for (int y = 1; y < 6; y++) for (int x = 1; x < 6; x++)
        sob_ok &= close_d(sx.at<float>(y, x), 80.0, 1e-4);   /* 8 * slope(10) */
    g.check(sob_ok, "Sobel-x of ramp(10x) != constant 80 interior");
    /* Sobel y of an x-ramp is 0 (no vertical variation). */
    cv::Mat sy; cv::Sobel(ramp, sy, CV_32F, 0, 1, 3, 1, 0, cv::BORDER_REPLICATE);
    bool sy_ok = true;
    for (int y = 1; y < 6; y++) for (int x = 1; x < 6; x++) sy_ok &= close_d(sy.at<float>(y, x), 0.0, 1e-4);
    g.check(sy_ok, "Sobel-y of x-ramp != 0");

    /* ---- boxFilter of a constant -> the same constant ---- */
    cv::Mat konst(8, 8, CV_32F, cv::Scalar(42.0));
    cv::Mat box; cv::boxFilter(konst, box, CV_32F, cv::Size(3, 3), cv::Point(-1, -1), true,
                               cv::BORDER_REPLICATE);
    bool box_ok = true;
    for (int y = 0; y < 8; y++) for (int x = 0; x < 8; x++) box_ok &= close_d(box.at<float>(y, x), 42.0, 1e-4);
    g.check(box_ok, "boxFilter(constant) != constant");
    /* blur() (normalized box) of a constant is also the constant. */
    cv::Mat blr; cv::blur(konst, blr, cv::Size(5, 5), cv::Point(-1, -1), cv::BORDER_REPLICATE);
    bool blr_ok = true;
    for (int y = 0; y < 8; y++) for (int x = 0; x < 8; x++) blr_ok &= close_d(blr.at<float>(y, x), 42.0, 1e-4);
    g.check(blr_ok, "blur(constant) != constant");

    /* ---- medianBlur removes a lone outlier, keeps the constant field ---- */
    cv::Mat med_in(7, 7, CV_8U, cv::Scalar(100));
    med_in.at<uchar>(3, 3) = 255;   /* one salt spike */
    cv::Mat med; cv::medianBlur(med_in, med, 3);
    g.check(med.at<uchar>(3, 3) == 100, "medianBlur did not remove lone outlier");
    bool med_flat = true;
    for (int y = 1; y < 6; y++) for (int x = 1; x < 6; x++) med_flat &= (med.at<uchar>(y, x) == 100);
    g.check(med_flat, "medianBlur disturbed the constant field");

    /* ---- filter2D with an identity kernel returns the input exactly ---- */
    cv::Mat idk = cv::Mat::zeros(3, 3, CV_32F); idk.at<float>(1, 1) = 1.0f;
    cv::Mat idout; cv::filter2D(ramp, idout, CV_32F, idk);
    bool id_ok = true;
    for (int y = 1; y < 6; y++) for (int x = 1; x < 6; x++)
        id_ok &= close_d(idout.at<float>(y, x), ramp.at<float>(y, x), 1e-4);
    g.check(id_ok, "filter2D(identity) != input");

    return g.finish();
}
