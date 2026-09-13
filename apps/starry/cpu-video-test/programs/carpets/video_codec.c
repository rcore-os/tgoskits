/* video_codec - transcode / round-trip matrix carpet (cell 2).
 *
 * Build a known synthetic source clip (smptebars, 30 frames, 320x240, 30fps) and run the codec x
 * container cartesian:
 *
 *     {ffv1(lossless), libx264, libx265, libvpx-vp9, mpeg2video} x {valid containers}
 *
 * For each combination: encode the source -> decode the first and middle frame back to raw rgb24 ->
 * assert in the pixel domain:
 *
 *   - ffv1 (lossless in the encoded yuv420p domain): the decoded frame is byte-identical to the
 *     reference decode of the same source through ffv1 (round-trip identity, sha256 equal).
 *   - lossy (h264/h265/vp9/mpeg2): PSNR of the decoded frame vs the yuv-reference frame is above a
 *     codec-appropriate floor, SSIM above a floor, and structure holds - a solid-color region stays
 *     that color within tolerance (no block artifacts turning a flat bar into noise).
 *   - container validity: each declared codec x container muxes and demuxes without error and the
 *     decoded geometry (w,h) is exactly the source geometry.
 *
 * The "reference" for the lossy PSNR is the source pushed once through yuv420p (color=... rawvideo)
 * so we measure codec loss, not colorspace conversion; ffv1's reference is the same yuv path, giving
 * an exact identity. All frames are generated fresh, so this cell needs no external asset.
 */
#include "video_common.h"

#define W 320
#define H 240
#define FPS 30
#define NF 30           /* 1.0 s */

static const char *TMP = "/tmp/videocodec";

/* Generate the synthetic source clip once as ffv1/rgb24 (lossless master) and also a yuv420p
 * reference decode of frame idx to raw rgb24. Returns 0 ok. */
static int make_source(const char *master) {
    char cmd[1024];
    snprintf(cmd, sizeof cmd,
        "ffmpeg -v error -y -f lavfi -i \"smptebars=size=%dx%d:rate=%d:duration=1\" "
        "-c:v ffv1 -pix_fmt yuv420p '%s'", W, H, FPS, master);
    return sh(cmd);
}

/* Decode frame idx of `in` to raw rgb24 at `out`. */
static int decode_frame(const char *in, int idx, const char *out) {
    char cmd[1400];
    snprintf(cmd, sizeof cmd,
        "ffmpeg -v error -y -i '%s' -vf \"select=eq(n\\,%d)\" -vframes 1 "
        "-f rawvideo -pix_fmt rgb24 '%s'", in, idx, out);
    return sh(cmd);
}

/* Solid-region structure check: the top-left SMPTE bar is a flat 75%% gray; assert its pixels stay
 * flat (low variance) in the decoded frame, i.e. the codec didn't shatter a flat area. */
static int flat_bar_ok(const frame *fr) {
    /* sample a 16x16 block well inside bar 0 (top-left) */
    long sx = 8, sy = 8; double m = 0; int n = 0;
    for (int y = 0; y < 16; y++)
        for (int x = 0; x < 16; x++) {
            long i = ((sy+y) * (long)fr->w + (sx+x)) * 3;
            m += rgb_luma(fr->px[i], fr->px[i+1], fr->px[i+2]); n++;
        }
    m /= n;
    double var = 0;
    for (int y = 0; y < 16; y++)
        for (int x = 0; x < 16; x++) {
            long i = ((sy+y) * (long)fr->w + (sx+x)) * 3;
            double d = rgb_luma(fr->px[i], fr->px[i+1], fr->px[i+2]) - m;
            var += d*d;
        }
    var /= n;
    /* flat 75%% gray ~ luma 190; assert mean in band and variance small */
    return m > 150 && m < 210 && var < 40.0;
}

int main(void) {
    gate g; gate_init(&g, "VIDEO_CODEC");
    char cmd[256]; snprintf(cmd, sizeof cmd, "mkdir -p %s", TMP); sh(cmd);

    char master[512]; snprintf(master, sizeof master, "%s/master.mkv", TMP);
    if (make_source(master) != 0) { gate_check(&g, 0, "source generate"); return gate_finish(&g); }

    /* yuv reference frames (frame 0 and middle) - what a perfect codec would reproduce */
    char ref0[512], refm[512];
    snprintf(ref0, sizeof ref0, "%s/ref0.rgb", TMP);
    snprintf(refm, sizeof refm, "%s/refm.rgb", TMP);
    int midf = NF / 2;
    gate_check(&g, decode_frame(master, 0, ref0) == 0, "ref frame0 decode");
    gate_check(&g, decode_frame(master, midf, refm) == 0, "ref mid decode");
    frame r0, rm;
    gate_check(&g, frame_read(ref0, W, H, 3, &r0) == 0, "ref0 geometry");
    gate_check(&g, frame_read(refm, W, H, 3, &rm) == 0, "refm geometry");
    unsigned char *r0y = frame_to_luma(&r0), *rmy = frame_to_luma(&rm);

    /* codec x container matrix. lossless flag drives byte-exact vs PSNR/SSIM floors. */
    struct { const char *name, *enc, *ext; int lossless; double psnr, ssim; } M[] = {
        {"ffv1",   "-c:v ffv1",                 "mkv",  1, 0,    0    },
        {"ffv1",   "-c:v ffv1",                 "avi",  1, 0,    0    },
        {"h264",   "-c:v libx264 -crf 18",      "mp4",  0, 38.0, 0.98 },
        {"h264",   "-c:v libx264 -crf 18",      "mkv",  0, 38.0, 0.98 },
        {"hevc",   "-c:v libx265 -crf 20",      "mp4",  0, 38.0, 0.98 },
        {"hevc",   "-c:v libx265 -crf 20",      "mkv",  0, 38.0, 0.98 },
        {"vp9",    "-c:v libvpx-vp9 -b:v 0 -crf 20", "webm", 0, 40.0, 0.98 },
        {"vp9",    "-c:v libvpx-vp9 -b:v 0 -crf 20", "mkv",  0, 40.0, 0.98 },
        {"mpeg2",  "-c:v mpeg2video -qscale:v 2", "mkv", 0, 36.0, 0.96 },
        {"mpeg2",  "-c:v mpeg2video -qscale:v 2", "mpg", 0, 36.0, 0.96 },
    };
    int nm = sizeof(M) / sizeof(M[0]);

    for (int i = 0; i < nm; i++) {
        char enc[512], d0[512], dm[512]; char ecmd[1400];
        snprintf(enc, sizeof enc, "%s/e_%s_%d.%s", TMP, M[i].name, i, M[i].ext);
        snprintf(d0,  sizeof d0,  "%s/d0_%s_%d.rgb", TMP, M[i].name, i);
        snprintf(dm,  sizeof dm,  "%s/dm_%s_%d.rgb", TMP, M[i].name, i);

        /* encode source -> codec/container (yuv420p in encoded domain) */
        snprintf(ecmd, sizeof ecmd, "ffmpeg -v error -y -i '%s' %s -pix_fmt yuv420p '%s'",
                 master, M[i].enc, enc);
        int rce = sh(ecmd);
        char tag[64]; snprintf(tag, sizeof tag, "%s x %s: encode", M[i].name, M[i].ext);
        gate_check(&g, rce == 0, tag);
        if (rce != 0) continue;

        /* decode first + middle frame back */
        int rd0 = decode_frame(enc, 0, d0), rdm = decode_frame(enc, midf, dm);
        snprintf(tag, sizeof tag, "%s x %s: decode", M[i].name, M[i].ext);
        gate_check(&g, rd0 == 0 && rdm == 0, tag);
        if (rd0 != 0 || rdm != 0) continue;

        frame f0, fm;
        int g0 = frame_read(d0, W, H, 3, &f0), gm = frame_read(dm, W, H, 3, &fm);
        snprintf(tag, sizeof tag, "%s x %s: geometry", M[i].name, M[i].ext);
        gate_check(&g, g0 == 0 && gm == 0, tag);   /* decoded WxH == source WxH */
        if (g0 != 0 || gm != 0) { frame_free(&f0); frame_free(&fm); continue; }

        if (M[i].lossless) {
            /* ffv1 is exact in the yuv domain: decoded frame == yuv reference frame, byte-exact */
            char h0[65], hr0[65], hm[65], hrm[65];
            sha256_buf(f0.px, f0.bytes, h0);   sha256_buf(r0.px, r0.bytes, hr0);
            sha256_buf(fm.px, fm.bytes, hm);   sha256_buf(rm.px, rm.bytes, hrm);
            snprintf(tag, sizeof tag, "%s x %s: lossless frame0 not byte-exact", M[i].name, M[i].ext);
            gate_check(&g, strcmp(h0, hr0) == 0, tag);
            snprintf(tag, sizeof tag, "%s x %s: lossless mid not byte-exact", M[i].name, M[i].ext);
            gate_check(&g, strcmp(hm, hrm) == 0, tag);
        } else {
            /* lossy: PSNR + SSIM floors vs yuv reference, plus flat-bar structure preserved */
            double p0 = psnr_bytes(f0.px, r0.px, f0.bytes);
            double pm = psnr_bytes(fm.px, rm.px, fm.bytes);
            unsigned char *f0y = frame_to_luma(&f0), *fmy = frame_to_luma(&fm);
            double s0 = ssim_luma(f0y, r0y, W, H);
            double sm = ssim_luma(fmy, rmy, W, H);
            free(f0y); free(fmy);
            snprintf(tag, sizeof tag, "%s x %s: PSNR floor (f0=%.1f mid=%.1f, need %.1f)",
                     M[i].name, M[i].ext, p0, pm, M[i].psnr);
            gate_check(&g, p0 >= M[i].psnr && pm >= M[i].psnr, tag);
            snprintf(tag, sizeof tag, "%s x %s: SSIM floor (f0=%.3f mid=%.3f, need %.2f)",
                     M[i].name, M[i].ext, s0, sm, M[i].ssim);
            gate_check(&g, s0 >= M[i].ssim && sm >= M[i].ssim, tag);
            snprintf(tag, sizeof tag, "%s x %s: flat bar shattered", M[i].name, M[i].ext);
            gate_check(&g, flat_bar_ok(&f0), tag);
        }
        frame_free(&f0); frame_free(&fm);
    }

    free(r0y); free(rmy);
    frame_free(&r0); frame_free(&rm);
    return gate_finish(&g);
}
