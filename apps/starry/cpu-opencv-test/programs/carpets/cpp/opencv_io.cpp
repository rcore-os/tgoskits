/* opencv_io - codec/container round-trips vs byte-exact / PSNR goldens.
 *
 * imencode/imdecode in-memory for PNG/BMP/PPM/TIFF/WebP (lossless -> byte-exact) and JPEG (lossy -> PSNR
 * bound); imwrite/imread file round-trip; VideoWriter+VideoCapture of a synthetic clip -> exact frame count
 * and first-frame content (FFV1 lossless). Real-asset leg reads images from ASSET_DIR (honest-skip if none).
 * Seeded RNG (0x233) for the test image so the bytes are fixed across arch.
 */
#include "cv_common.h"
#include <opencv2/imgcodecs.hpp>
#include <opencv2/videoio.hpp>
#include <opencv2/imgproc.hpp>
#include <vector>
#include <string>
#include <cstdlib>
#include <sys/stat.h>
#include <dirent.h>

static double psnr(const cv::Mat &a, const cv::Mat &b) {
    cv::Mat d; cv::absdiff(a, b, d); d.convertTo(d, CV_32F); d = d.mul(d);
    double mse = cv::mean(d)[0] + cv::mean(d)[1] + cv::mean(d)[2];
    mse /= 3.0;
    if (mse <= 1e-10) return 1e9;
    return 10.0 * std::log10(255.0 * 255.0 / mse);
}

int main() {
    Gate g("OPENCV_IO");
    cv::setNumThreads(1);

    /* fixed seeded test image (0x233). */
    cv::RNG rng(0x233);
    cv::Mat img(24, 32, CV_8UC3);
    rng.fill(img, cv::RNG::UNIFORM, 0, 256);

    /* lossless in-memory round-trips: decoded bytes must equal the source exactly. */
    struct { const char *ext; } lossless[] = {{".png"}, {".bmp"}, {".ppm"}, {".tiff"}, {".webp"}};
    for (auto &f : lossless) {
        std::vector<uchar> buf;
        bool enc = cv::imencode(f.ext, img, buf);
        if (!enc || buf.empty()) { g.check(false, f.ext); continue; }
        cv::Mat dec = cv::imdecode(buf, cv::IMREAD_COLOR);
        bool exact = (!dec.empty() && dec.size() == img.size() &&
                      cv::countNonZero(dec.reshape(1) != img.reshape(1)) == 0);
        g.check(exact, (std::string("lossless round-trip not byte-exact: ") + f.ext).c_str());
    }

    /* JPEG lossy: use a smooth gradient (representative photographic content; pure noise defeats DCT and is
       not a meaningful PSNR target). q95 must reconstruct with high PSNR but is not byte-exact. */
    {
        cv::Mat grad(24, 32, CV_8UC3);
        for (int y = 0; y < 24; y++) for (int x = 0; x < 32; x++)
            grad.at<cv::Vec3b>(y, x) = cv::Vec3b((uchar)(x * 8), (uchar)(y * 10), (uchar)((x + y) * 4));
        std::vector<uchar> buf;
        std::vector<int> params = {cv::IMWRITE_JPEG_QUALITY, 95};
        bool enc = cv::imencode(".jpg", grad, buf, params);
        cv::Mat dec = enc ? cv::imdecode(buf, cv::IMREAD_COLOR) : cv::Mat();
        g.check(enc && !dec.empty() && dec.size() == grad.size(), "JPEG encode/decode failed");
        g.check(!dec.empty() && psnr(grad, dec) > 35.0, "JPEG q95 PSNR below 35 dB on a gradient");
        g.check(!dec.empty() && cv::countNonZero(dec.reshape(1) != grad.reshape(1)) > 0,
                "JPEG unexpectedly byte-exact (should be lossy)");
    }

    /* PGM (gray) lossless round-trip on a single-channel image. */
    {
        cv::Mat gray; cv::cvtColor(img, gray, cv::COLOR_BGR2GRAY);
        std::vector<uchar> buf;
        bool enc = cv::imencode(".pgm", gray, buf);
        cv::Mat dec = enc ? cv::imdecode(buf, cv::IMREAD_GRAYSCALE) : cv::Mat();
        g.check(enc && !dec.empty() && cv::countNonZero(dec != gray) == 0, "PGM gray round-trip not exact");
    }

    /* IMREAD_GRAYSCALE decode of a color PNG yields the BT.601 gray of the image. */
    {
        std::vector<uchar> buf; cv::imencode(".png", img, buf);
        cv::Mat gdec = cv::imdecode(buf, cv::IMREAD_GRAYSCALE);
        cv::Mat gref; cv::cvtColor(img, gref, cv::COLOR_BGR2GRAY);
        g.check(!gdec.empty() && cv::countNonZero(cv::abs(gdec - gref) > 1) == 0,
                "IMREAD_GRAYSCALE decode != BT.601 gray");
    }

    /* imwrite/imread file round-trip through a writable temp dir. */
    const char *tmp = getenv("TMPDIR"); std::string tdir = tmp ? tmp : "/tmp";
    {
        std::string p = tdir + "/cvio_rt.png";
        bool w = cv::imwrite(p, img);
        cv::Mat rd = cv::imread(p, cv::IMREAD_COLOR);
        g.check(w && !rd.empty() && cv::countNonZero(rd.reshape(1) != img.reshape(1)) == 0,
                "imwrite/imread PNG file round-trip not exact");
        remove(p.c_str());
    }

    /* VideoWriter + VideoCapture: synthetic 5-frame clip, FFV1 lossless -> exact frame count + first-frame
       content. If no working writer is available, honest-skip the video legs. */
    {
        std::string vp = tdir + "/cvio_clip.avi";
        int fourcc = cv::VideoWriter::fourcc('F', 'F', 'V', '1');
        cv::VideoWriter vw(vp, fourcc, 10.0, cv::Size(32, 32), true);
        if (!vw.isOpened()) {
            g.skip("no FFV1 VideoWriter available - video legs skipped");
        } else {
            std::vector<cv::Mat> frames;
            for (int i = 0; i < 5; i++) {
                cv::Mat fr(32, 32, CV_8UC3, cv::Scalar(i * 10, 0, 0));
                fr.at<cv::Vec3b>(0, 0) = cv::Vec3b(i, i + 1, i + 2);
                frames.push_back(fr.clone());
                vw.write(fr);
            }
            vw.release();
            cv::VideoCapture cap(vp);
            g.check(cap.isOpened(), "VideoCapture failed to open the written clip");
            int cnt = (int)cap.get(cv::CAP_PROP_FRAME_COUNT);
            g.check(cnt == 5, "clip frame count != 5");
            cv::Mat f0; bool got = cap.read(f0);
            g.check(got && !f0.empty(), "could not read first frame");
            g.check(got && f0.at<cv::Vec3b>(0, 0) == cv::Vec3b(0, 1, 2),
                    "first-frame marker pixel != (0,1,2)");
            g.check(got && std::abs((int)cv::mean(f0)[0] - 0) <= 1, "first-frame B mean != 0");
            cap.release();
            remove(vp.c_str());
        }
    }

    /* Real-asset leg: read every image under ASSET_DIR and assert it decodes to a nonempty 3-channel Mat.
       Honest-skip if ASSET_DIR is absent or empty. */
    const char *ad = getenv("ASSET_DIR");
    int read_assets = 0;
    if (ad) {
        DIR *d = opendir(ad);
        if (d) {
            struct dirent *e;
            while ((e = readdir(d))) {
                std::string n = e->d_name;
                auto ends = [&](const char *s){ return n.size() >= strlen(s) &&
                    n.compare(n.size() - strlen(s), strlen(s), s) == 0; };
                if (ends(".png") || ends(".jpg") || ends(".bmp") || ends(".ppm") || ends(".tiff")) {
                    cv::Mat m = cv::imread(std::string(ad) + "/" + n, cv::IMREAD_COLOR);
                    g.check(!m.empty() && m.channels() == 3,
                            (std::string("asset failed to decode: ") + n).c_str());
                    read_assets++;
                }
            }
            closedir(d);
        }
    }
    if (read_assets == 0) g.skip("no images under ASSET_DIR - real-asset leg honest-skipped");

    return g.finish();
}
