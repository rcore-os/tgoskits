/* opencv_mat - cv::Mat arithmetic & structure vs element-exact closed form.
 *
 * add / subtract / multiply (per-element) / matmul (gemm) / transpose on KNOWN small matrices, asserted
 * element-exact against a hand-computed golden; plus Mat type/channels/ROI/reshape semantics. No RNG.
 */
#include "cv_common.h"
#include <opencv2/imgproc.hpp>

int main() {
    cv::setNumThreads(1);
    Gate g("OPENCV_MAT");

    /* Known 2x3 / 3x2 integer matrices. */
    cv::Mat A = (cv::Mat_<double>(2, 3) << 1, 2, 3, 4, 5, 6);
    cv::Mat B = (cv::Mat_<double>(2, 3) << 6, 5, 4, 3, 2, 1);

    /* element-wise add: A+B == all 7s */
    cv::Mat S = A + B;
    bool add_ok = true;
    for (int i = 0; i < 2; i++) for (int j = 0; j < 3; j++)
        add_ok &= close_d(S.at<double>(i, j), 7.0, 1e-9);
    g.check(add_ok, "A+B != 7 everywhere");

    /* element-wise subtract: A-B == [-5,-3,-1;1,3,5] */
    cv::Mat D = A - B;
    double sub_gold[2][3] = {{-5, -3, -1}, {1, 3, 5}};
    bool sub_ok = true;
    for (int i = 0; i < 2; i++) for (int j = 0; j < 3; j++)
        sub_ok &= close_d(D.at<double>(i, j), sub_gold[i][j], 1e-9);
    g.check(sub_ok, "A-B mismatch");

    /* element-wise multiply (Hadamard): A.mul(B) == [6,10,12;12,10,6] */
    cv::Mat M = A.mul(B);
    double mul_gold[2][3] = {{6, 10, 12}, {12, 10, 6}};
    bool mul_ok = true;
    for (int i = 0; i < 2; i++) for (int j = 0; j < 3; j++)
        mul_ok &= close_d(M.at<double>(i, j), mul_gold[i][j], 1e-9);
    g.check(mul_ok, "A.mul(B) mismatch");

    /* scalar multiply: 2*A */
    cv::Mat A2 = 2.0 * A;
    bool s2_ok = true;
    for (int i = 0; i < 2; i++) for (int j = 0; j < 3; j++)
        s2_ok &= close_d(A2.at<double>(i, j), 2 * A.at<double>(i, j), 1e-9);
    g.check(s2_ok, "2*A mismatch");

    /* matmul (gemm): A(2x3) * A^T(3x2) = [[14,32],[32,77]] */
    cv::Mat G = A * A.t();
    double mm_gold[2][2] = {{14, 32}, {32, 77}};
    bool mm_ok = (G.rows == 2 && G.cols == 2);
    for (int i = 0; i < 2 && mm_ok; i++) for (int j = 0; j < 2; j++)
        mm_ok &= close_d(G.at<double>(i, j), mm_gold[i][j], 1e-9);
    g.check(mm_ok, "A*A^T (gemm) mismatch");

    /* transpose: A^T is 3x2 with A^T(j,i)==A(i,j) */
    cv::Mat T = A.t();
    bool t_ok = (T.rows == 3 && T.cols == 2);
    for (int i = 0; i < 2 && t_ok; i++) for (int j = 0; j < 3; j++)
        t_ok &= close_d(T.at<double>(j, i), A.at<double>(i, j), 1e-9);
    g.check(t_ok, "transpose mismatch");

    /* determinant of a known 2x2: det([[1,2],[3,4]]) = -2 */
    cv::Mat K = (cv::Mat_<double>(2, 2) << 1, 2, 3, 4);
    g.check(close_d(cv::determinant(K), -2.0, 1e-9), "det != -2");

    /* inverse: K * K^-1 == I */
    cv::Mat Ki = K.inv();
    cv::Mat I2 = K * Ki;
    bool inv_ok = close_d(I2.at<double>(0, 0), 1, 1e-9) && close_d(I2.at<double>(1, 1), 1, 1e-9) &&
                  close_d(I2.at<double>(0, 1), 0, 1e-9) && close_d(I2.at<double>(1, 0), 0, 1e-9);
    g.check(inv_ok, "K*K^-1 != I");

    /* type / channels: a 3-channel 8U image */
    cv::Mat C8 = cv::Mat::zeros(4, 5, CV_8UC3);
    g.check(C8.type() == CV_8UC3, "type != CV_8UC3");
    g.check(C8.channels() == 3, "channels != 3");
    g.check(C8.rows == 4 && C8.cols == 5, "shape != 4x5");
    g.check(C8.elemSize() == 3, "elemSize != 3 bytes");

    /* ROI is a view: writing a ROI mutates the parent at the mapped location and nowhere else. */
    cv::Mat P = cv::Mat::zeros(6, 6, CV_8UC1);
    cv::Mat roi = P(cv::Rect(1, 2, 3, 2));   /* x=1,y=2,w=3,h=2 */
    roi.setTo(200);
    bool roi_ok = true;
    for (int y = 0; y < 6; y++) for (int x = 0; x < 6; x++) {
        int expect = (x >= 1 && x < 4 && y >= 2 && y < 4) ? 200 : 0;
        roi_ok &= (P.at<uchar>(y, x) == expect);
    }
    g.check(roi_ok, "ROI write did not map back to parent exactly");

    /* reshape: 12 contiguous values reshaped 1x12 -> 3x4 keeps row-major order. */
    cv::Mat lin(1, 12, CV_32S);
    for (int i = 0; i < 12; i++) lin.at<int>(0, i) = i;
    cv::Mat r34 = lin.reshape(1, 3);
    bool rs_ok = (r34.rows == 3 && r34.cols == 4);
    for (int i = 0; i < 3 && rs_ok; i++) for (int j = 0; j < 4; j++)
        rs_ok &= (r34.at<int>(i, j) == i * 4 + j);
    g.check(rs_ok, "reshape row-major order wrong");

    /* countNonZero / min-max on a known matrix. */
    cv::Mat mm = (cv::Mat_<uchar>(2, 3) << 0, 5, 0, 9, 0, 3);
    g.check(cv::countNonZero(mm) == 3, "countNonZero != 3");
    double mn, mx; cv::minMaxLoc(mm, &mn, &mx);
    g.check(close_d(mn, 0, 1e-9) && close_d(mx, 9, 1e-9), "minMax != (0,9)");

    return g.finish();
}
