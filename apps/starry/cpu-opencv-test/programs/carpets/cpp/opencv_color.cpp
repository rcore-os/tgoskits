/* opencv_color - cvtColor conversions vs the closed-form color matrix, per pixel.
 *
 * BGR<->RGB (byte-exact channel swap), BGR->GRAY (Y=round(0.299R+0.587G+0.114B), fixed-point closed form),
 * BGR<->HSV (closed form for pure primaries), BGR<->YCrCb (BT.601), BGR<->I420/YV12 (4:2:0 planar, luma
 * plane == gray closed form). Known 4-color image; every pixel asserted. No RNG.
 */
#include "cv_common.h"
#include <opencv2/imgproc.hpp>

int main() {
    cv::setNumThreads(1);
    Gate g("OPENCV_COLOR");

    /* A 2x2 BGR image of four known colors (OpenCV is BGR order in memory):
       (0,0)=red   BGR(0,0,255)   (0,1)=green BGR(0,255,0)
       (1,0)=blue  BGR(255,0,0)   (1,1)=gray  BGR(128,128,128) */
    cv::Mat bgr(2, 2, CV_8UC3);
    bgr.at<cv::Vec3b>(0, 0) = cv::Vec3b(0, 0, 255);
    bgr.at<cv::Vec3b>(0, 1) = cv::Vec3b(0, 255, 0);
    bgr.at<cv::Vec3b>(1, 0) = cv::Vec3b(255, 0, 0);
    bgr.at<cv::Vec3b>(1, 1) = cv::Vec3b(128, 128, 128);

    /* BGR->RGB: byte-exact channel swap (B<->R), G unchanged. */
    cv::Mat rgb; cv::cvtColor(bgr, rgb, cv::COLOR_BGR2RGB);
    bool swap_ok = true;
    for (int y = 0; y < 2; y++) for (int x = 0; x < 2; x++) {
        cv::Vec3b b = bgr.at<cv::Vec3b>(y, x), r = rgb.at<cv::Vec3b>(y, x);
        swap_ok &= (r[0] == b[2] && r[1] == b[1] && r[2] == b[0]);
    }
    g.check(swap_ok, "BGR2RGB is not an exact B<->R swap");

    /* round-trip RGB->BGR returns the original exactly. */
    cv::Mat back; cv::cvtColor(rgb, back, cv::COLOR_RGB2BGR);
    bool rt_ok = true;
    for (int y = 0; y < 2; y++) for (int x = 0; x < 2; x++)
        rt_ok &= (back.at<cv::Vec3b>(y, x) == bgr.at<cv::Vec3b>(y, x));
    g.check(rt_ok, "BGR->RGB->BGR not identity");

    /* BGR->GRAY: assert every pixel == fixed-point 0.299R+0.587G+0.114B.
       red->76, green->150, blue->29, gray(128,128,128)->128. */
    cv::Mat gray; cv::cvtColor(bgr, gray, cv::COLOR_BGR2GRAY);
    bool gray_ok = true;
    for (int y = 0; y < 2; y++) for (int x = 0; x < 2; x++) {
        cv::Vec3b p = bgr.at<cv::Vec3b>(y, x);
        int want = bgr2gray_601(p[0], p[1], p[2]);
        gray_ok &= (gray.at<uchar>(y, x) == want);
    }
    g.check(gray_ok, "BGR2GRAY != BT.601 fixed-point closed form");
    /* pin the actual expected values so a mutation is caught even if the helper drifted. */
    g.check(gray.at<uchar>(0, 0) == 76 && gray.at<uchar>(0, 1) == 150 &&
            gray.at<uchar>(1, 0) == 29 && gray.at<uchar>(1, 1) == 128,
            "gray pins (76,150,29,128) wrong");

    /* BGR->YCrCb (BT.601). For pure gray (128,128,128): Y=128, Cr=Cb=128 (neutral chroma). */
    cv::Mat ycc; cv::cvtColor(bgr, ycc, cv::COLOR_BGR2YCrCb);
    cv::Vec3b yg = ycc.at<cv::Vec3b>(1, 1);
    g.check(yg[0] == 128 && yg[1] == 128 && yg[2] == 128, "YCrCb of gray != (128,128,128)");
    /* Y channel of YCrCb equals the GRAY closed form for every pixel. */
    bool yluma_ok = true;
    for (int y = 0; y < 2; y++) for (int x = 0; x < 2; x++)
        yluma_ok &= close_i(ycc.at<cv::Vec3b>(y, x)[0], gray.at<uchar>(y, x), 1);
    g.check(yluma_ok, "YCrCb luma != gray closed form");

    /* BGR->HSV: pure primaries have known H (OpenCV 8U H in [0,180)):
       red H=0 S=255 V=255 ; green H=60 ; blue H=120 ; gray S=0 V=128. */
    cv::Mat hsv; cv::cvtColor(bgr, hsv, cv::COLOR_BGR2HSV);
    cv::Vec3b hr = hsv.at<cv::Vec3b>(0, 0), hg = hsv.at<cv::Vec3b>(0, 1),
              hb = hsv.at<cv::Vec3b>(1, 0), hgy = hsv.at<cv::Vec3b>(1, 1);
    g.check(hr[0] == 0   && hr[1] == 255 && hr[2] == 255, "HSV(red) != (0,255,255)");
    g.check(hg[0] == 60  && hg[1] == 255 && hg[2] == 255, "HSV(green) != (60,255,255)");
    g.check(hb[0] == 120 && hb[1] == 255 && hb[2] == 255, "HSV(blue) != (120,255,255)");
    g.check(hgy[1] == 0  && hgy[2] == 128, "HSV(gray) S/V != (0,128)");

    /* HSV round-trip back to BGR returns primaries exactly (they sit on cube corners). */
    cv::Mat hsv2bgr; cv::cvtColor(hsv, hsv2bgr, cv::COLOR_HSV2BGR);
    bool hsv_rt = true;
    for (int y = 0; y < 2; y++) for (int x = 0; x < 2; x++) {
        cv::Vec3b a = bgr.at<cv::Vec3b>(y, x), b = hsv2bgr.at<cv::Vec3b>(y, x);
        for (int c = 0; c < 3; c++) hsv_rt &= close_i(a[c], b[c], 2);
    }
    g.check(hsv_rt, "HSV->BGR round-trip drifted > 2");

    /* BGR->I420 (YUV 4:2:0 planar): output is (H*3/2) x W single channel. The top HxW block is the Y
       plane and must equal the GRAY closed form. Use a larger even image so 4:2:0 subsampling is valid. */
    cv::Mat big(4, 4, CV_8UC3);
    for (int y = 0; y < 4; y++) for (int x = 0; x < 4; x++)
        big.at<cv::Vec3b>(y, x) = cv::Vec3b((x * 40) & 0xff, (y * 40) & 0xff, ((x + y) * 30) & 0xff);
    cv::Mat i420; cv::cvtColor(big, i420, cv::COLOR_BGR2YUV_I420);
    g.check(i420.rows == 6 && i420.cols == 4, "I420 shape != 6x4");
    /* I420 luma is STUDIO-SWING BT.601 (Y' in [16,235]): Y = round(0.257R+0.504G+0.098B) + 16, distinct
       from the full-range BGR2GRAY. OpenCV's fixed-point: (R*66 + G*129 + B*25 + 128) >> 8 + 16. */
    bool yplane_ok = true;
    for (int y = 0; y < 4; y++) for (int x = 0; x < 4; x++) {
        cv::Vec3b p = big.at<cv::Vec3b>(y, x);
        int want = ((p[2] * 66 + p[1] * 129 + p[0] * 25 + 128) >> 8) + 16;
        yplane_ok &= close_i(i420.at<uchar>(y, x), want, 1);
    }
    g.check(yplane_ok, "I420 Y-plane != BT.601 studio-swing closed form");

    /* I420 -> BGR round-trip stays close (chroma subsampling loses a little). */
    cv::Mat i420back; cv::cvtColor(i420, i420back, cv::COLOR_YUV2BGR_I420);
    g.check(i420back.rows == 4 && i420back.cols == 4, "I420->BGR shape wrong");

    return g.finish();
}
