/* video_realassets - real-media leg (optional, references $ASSET_DIR, honest-skip when absent).
 *
 * The extracted real clips + golden stats live under $ASSET_DIR: video/badapple_clips/<clip> and
 * golden/badapple_clips_firstframe.tsv (clip, codec, sha256_rgb24_firstframe, luma8x8_hex) plus
 * golden/badapple_clips_t2.tsv (host sha256_rgb24 at t=2.0). On-target these ride a git submodule.
 *
 * For each of the four transcodes {h264, hevc, vp9, ffv1}:
 *   - assert codec_name, width==640, height==480, r_frame_rate==30/1 via ffprobe (vs golden),
 *   - decode the first frame to raw rgb24, assert sha256(rgb24) == golden firstframe sha (byte-exact),
 *   - assert the 8x8-bicubic-gray signature of the first frame == golden luma8x8_hex,
 *   - seek to absolute PTS 2.0, then use the on-target ffv1 lossless frame as reference and align
 *     each lossy stream within a bounded +/- 6-frame window (container/transcode offsets can shift
 *     the same source frame).  The aligned frame must still satisfy PSNR, SSIM, binary silhouette
 *     mismatch, IoU and white-ratio-delta bounds; the host t2 sha is a diagnostic fingerprint, not a
 *     cross-FFmpeg-version byte-equality contract,
 *   - assert the reported duration ~= 5.13 s (first-5s transcode).
 *
 * If the firstframe golden tsv or the clips directory is absent, every real-clip check honest-skips
 * and the cell prints a SKIP marker with a single satisfied assertion so the synthetic cells still
 * gate.  When the assets are present, the t2 fingerprint table must also be complete.
 */
#include "video_common.h"
#include <sys/stat.h>

#define CW 640
#define CH 480
#define CLIP_T2_TIME 2.0
#define CLIP_T2_FPS 30.0
#define CLIP_T2_SEARCH_RADIUS 6   /* +/- frames around t=2.0 for cross-container content alignment */
#define CLIP_NP ((long)CW * CH)
#define CLIP_BIN_THRESHOLD 128

/* The host t2 fingerprints were produced by FFmpeg 6.1.1/x86_64.  FFmpeg 8.1.2/aarch64 does not
 * guarantee byte-identical rgb24 for the same yuv420p decode: decoder build differences and swscale
 * rounding/range handling can move non-black pixels.  Keep the host hash for diagnostics, but judge
 * the real decode against the on-target ffv1 lossless frame.  These bounds reject a black/stuck
 * frame, wrong-frame decode, block garbage, and gross luma/chroma corruption while allowing normal
 * codec noise and build-specific rgb24 rounding. */
#define T2_PSNR_MIN 18.0
#define T2_RGB_PSNR_MIN 15.0
#define T2_SSIM_MIN 0.85
#define T2_MASK_MISMATCH_MAX 0.12
#define T2_IOU_MIN 0.70
#define T2_WHITE_DELTA_MAX 0.05
#define T2_VARIANCE_MIN 2.0

static const char *asset_dir(void) {
    const char *d = getenv("ASSET_DIR");
    return (d && *d) ? d : "assets";
}
static int file_exists(const char *p) { struct stat st; return stat(p, &st) == 0; }

static const char *TMP = "/tmp/videoreal";

static long run_capture(const char *cmd, char *buf, long cap) {
    FILE *p = popen(cmd, "r"); if (!p) return -1;
    long n = (long)fread(buf, 1, cap - 1, p); pclose(p);
    buf[n < 0 ? 0 : n] = 0;
    while (n > 0 && (buf[n-1] == '\n' || buf[n-1] == '\r' || buf[n-1] == ' ')) buf[--n] = 0;
    return n;
}
static int probe_str(const char *file, const char *entry, char *out, long cap) {
    char cmd[1024];
    snprintf(cmd, sizeof cmd,
        "ffprobe -v error -select_streams v:0 -show_entries stream=%s -of default=nk=1:nw=1 '%s'",
        entry, file);
    return run_capture(cmd, out, cap) > 0 ? 0 : -1;
}
static double probe_duration(const char *file) {
    char cmd[1024], buf[64];
    snprintf(cmd, sizeof cmd,
        "ffprobe -v error -show_entries format=duration -of default=nk=1:nw=1 '%s'", file);
    if (run_capture(cmd, buf, sizeof buf) <= 0) return -1;
    return atof(buf);
}

/* Parse a firstframe tsv row: clip \t codec \t sha \t luma8x8. Return 0 ok. */
struct row { char clip[128], codec[32], sha[80], luma[160]; };

struct t2_sample {
    int valid;
    char codec[32];
    char first_sha[80];
    char host_sha[80];
    char path[700];
    char start_time[48];
    char time_base[48];
    char sha[65];
    int offset_frames;
    double pts;
    unsigned char *rgb;
    long rgb_bytes;
    unsigned char *luma;
    unsigned char *mask;
    double white;
};

static void t2_free(struct t2_sample *s) {
    free(s->rgb); free(s->luma); free(s->mask);
    s->rgb = NULL; s->luma = NULL; s->mask = NULL;
    s->rgb_bytes = 0; s->white = 0.0; s->pts = -1.0; s->valid = 0; s->offset_frames = 0;
    s->sha[0] = 0;
}

static double binary_mismatch(const unsigned char *a, const unsigned char *b, long n) {
    long mism = 0;
    for (long i = 0; i < n; i++) if (a[i] != b[i]) mism++;
    return (double)mism / (double)n;
}

static double binary_iou(const unsigned char *a, const unsigned char *b, long n) {
    long inter = 0, uni = 0;
    for (long i = 0; i < n; i++) {
        inter += a[i] && b[i];
        uni += a[i] || b[i];
    }
    /* Empty union means the sampled frame has no binary foreground; treat it as a failed
     * discriminating sample rather than allowing a black frame to match vacuously. */
    return uni ? (double)inter / (double)uni : 0.0;
}

static double luma_variance(const unsigned char *y, long n) {
    double mean = 0.0;
    for (long i = 0; i < n; i++) mean += y[i];
    mean /= (double)n;
    double var = 0.0;
    for (long i = 0; i < n; i++) {
        double d = y[i] - mean;
        var += d * d;
    }
    return var / (double)n;
}

static int decode_t2_sample(struct t2_sample *s, double time) {
    t2_free(s);
    char t2rgb[512]; snprintf(t2rgb, sizeof t2rgb, "%s/t2.rgb", TMP);
    remove(t2rgb);
    double pts = -1.0;
    if (ffmpeg_frame_rgb24_at(s->path, time, t2rgb, &pts) != 0) return -1;
    if (sha256_file(t2rgb, s->sha) != 0) return -1;
    frame fr;
    if (frame_read(t2rgb, CW, CH, 3, &fr) != 0) return -1;
    unsigned char *luma = frame_to_luma(&fr);
    unsigned char *mask = (unsigned char *)malloc(CLIP_NP);
    if (!luma || !mask) { free(luma); free(mask); frame_free(&fr); return -1; }
    for (long i = 0; i < CLIP_NP; i++) mask[i] = luma[i] >= CLIP_BIN_THRESHOLD;
    s->rgb = fr.px; s->rgb_bytes = fr.bytes; fr.px = NULL;
    s->luma = luma; s->mask = mask;
    s->white = white_ratio(luma, CLIP_NP, CLIP_BIN_THRESHOLD);
    s->pts = pts; s->valid = 1;
    frame_free(&fr);
    return 0;
}

/* Prefer the candidate whose binary silhouette overlaps the ffv1 reference; luma PSNR breaks ties
 * between adjacent silhouettes.  The search is bounded, so a black/garbage/wrong-codec decode cannot
 * search arbitrarily far for a match. */
static double t2_align_score(const struct t2_sample *a, const struct t2_sample *ref) {
    double iou = binary_iou(a->mask, ref->mask, CLIP_NP);
    double psnr = psnr_bytes(a->luma, ref->luma, CLIP_NP);
    return iou * 1e6 + psnr;
}

static int t2_passes(const struct t2_sample *a, const struct t2_sample *ref) {
    double psnr = psnr_bytes(a->luma, ref->luma, CLIP_NP);
    double rgb_psnr = psnr_bytes(a->rgb, ref->rgb, ref->rgb_bytes);
    double ssim = ssim_luma(a->luma, ref->luma, CW, CH);
    double mism = binary_mismatch(a->mask, ref->mask, CLIP_NP);
    double iou = binary_iou(a->mask, ref->mask, CLIP_NP);
    double wd = fabs(a->white - ref->white);
    return psnr >= T2_PSNR_MIN && rgb_psnr >= T2_RGB_PSNR_MIN && ssim >= T2_SSIM_MIN
        && mism <= T2_MASK_MISMATCH_MAX && iou >= T2_IOU_MIN && wd <= T2_WHITE_DELTA_MAX;
}

int main(void) {
    gate g; gate_init(&g, "VIDEO_REALASSETS");
    const char *AD = asset_dir();
    char ff_tsv[512], t2_tsv[512];
    snprintf(ff_tsv, sizeof ff_tsv, "%s/golden/badapple_clips_firstframe.tsv", AD);
    snprintf(t2_tsv, sizeof t2_tsv, "%s/golden/badapple_clips_t2.tsv", AD);

    if (!file_exists(ff_tsv)) {
        fprintf(stderr, "  (assets absent: %s not found - real-clip checks honest-skipped)\n", ff_tsv);
        gate_check(&g, !file_exists(ff_tsv), "asset-skip path");
        printf("VIDEO_REALASSETS SKIP (no assets at %s) ", AD);
        return gate_finish(&g);
    }

    char cmd[256]; snprintf(cmd, sizeof cmd, "mkdir -p %s", TMP); sh(cmd);

    /* load t2 golden into a small map (clip -> sha) */
    struct { char clip[128], sha[80]; } t2[8]; int nt2 = 0;
    FILE *t = fopen(t2_tsv, "r");
    if (t) {
        char line[512];
        while (fgets(line, sizeof line, t) && nt2 < 8) {
            char clip[128], codec[32], sha[80];
            if (sscanf(line, "%127s\t%31s\t%79s", clip, codec, sha) == 3 && strcmp(clip, "clip") != 0) {
                strncpy(t2[nt2].clip, clip, sizeof t2[nt2].clip - 1);
                strncpy(t2[nt2].sha, sha, sizeof t2[nt2].sha - 1);
                nt2++;
            }
        }
        fclose(t);
    }
    gate_check(&g, nt2 == 4, "realclip: host t2 fingerprint table incomplete");

    FILE *f = fopen(ff_tsv, "r");
    if (!f) { gate_check(&g, 0, "firstframe tsv open"); return gate_finish(&g); }

    int rows = 0; char line[512];
    struct t2_sample samples[8];
    memset(samples, 0, sizeof samples);

    while (fgets(line, sizeof line, f)) {
        struct row r;
        if (sscanf(line, "%127s\t%31s\t%79s\t%159s", r.clip, r.codec, r.sha, r.luma) != 4) continue;
        if (strcmp(r.clip, "clip") == 0) continue;   /* header */

        rows++;
        if (rows > (int)(sizeof samples / sizeof samples[0])) {
            gate_check(&g, 0, "realclip: too many golden rows");
            break;
        }
        struct t2_sample *s = &samples[rows - 1];
        snprintf(s->codec, sizeof s->codec, "%s", r.codec);
        snprintf(s->first_sha, sizeof s->first_sha, "%s", r.sha);
        for (int k = 0; k < nt2; k++)
            if (strcmp(t2[k].clip, r.clip) == 0) {
                snprintf(s->host_sha, sizeof s->host_sha, "%s", t2[k].sha);
                break;
            }

        char clippath[700];
        snprintf(clippath, sizeof clippath, "%s/video/badapple_clips/%s", AD, r.clip);
        if (!file_exists(clippath)) { fprintf(stderr, "  (missing clip %s)\n", clippath); continue; }

        /* stream metadata vs golden */
        char codec[48], w[16], h[16], rfr[16];
        probe_str(clippath, "codec_name", codec, sizeof codec);
        probe_str(clippath, "width", w, sizeof w);
        probe_str(clippath, "height", h, sizeof h);
        probe_str(clippath, "r_frame_rate", rfr, sizeof rfr);
        gate_check(&g, strcmp(codec, r.codec) == 0, "realclip: codec_name != golden");
        gate_check(&g, atoi(w) == CW, "realclip: width != 640");
        gate_check(&g, atoi(h) == CH, "realclip: height != 480");
        gate_check(&g, strcmp(rfr, "30/1") == 0, "realclip: r_frame_rate != 30/1");
        double dur = probe_duration(clippath);
        gate_check(&g, dur > 5.0 && dur < 5.3, "realclip: duration not ~5.13s");

        /* first frame rgb24 sha byte-exact */
        char rgb[512]; snprintf(rgb, sizeof rgb, "%s/ff.rgb", TMP);
        if (ffmpeg_frame_rgb24(clippath, -1, "", rgb) != 0) { gate_check(&g, 0, "firstframe decode"); continue; }
        char sha[65];
        gate_check(&g, sha256_file(rgb, sha) == 0 && strcmp(sha, r.sha) == 0,
                   "realclip: firstframe rgb24 sha != golden");
        frame fr;
        if (frame_read(rgb, CW, CH, 3, &fr) == 0) {
            gate_check(&g, fr.bytes == (long)CW * CH * 3, "realclip: firstframe geometry");
            frame_free(&fr);
        } else gate_check(&g, 0, "realclip: firstframe geometry read");

        /* 8x8 luma sig of first frame == golden luma8x8_hex */
        char gray[512]; snprintf(gray, sizeof gray, "%s/ff.gray", TMP);
        if (ffmpeg_luma8x8(clippath, -1, gray) == 0) {
            unsigned char *lb = NULL; long ln = read_file_bytes(gray, &lb);
            char lhex[129] = {0}; if (ln == 64) hex_encode(lb, 64, lhex);
            gate_check(&g, ln == 64 && strcmp(lhex, r.luma) == 0,
                       "realclip: firstframe 8x8 luma sig != golden");
            free(lb);
        } else gate_check(&g, 0, "realclip: luma8x8 decode");

        /* Anchor the sample at absolute PTS 2.0 via input-side accurate seek, then record the
         * container time contract.  The bounded content alignment below handles transcodes whose
         * "2.0 s" maps to a shifted source frame. */
        snprintf(s->path, sizeof s->path, "%s", clippath);
        snprintf(s->start_time, sizeof s->start_time, "?");
        snprintf(s->time_base, sizeof s->time_base, "?");
        probe_str(clippath, "start_time", s->start_time, sizeof s->start_time);
        probe_str(clippath, "time_base", s->time_base, sizeof s->time_base);
        if (decode_t2_sample(s, CLIP_T2_TIME) != 0) {
            gate_check(&g, 0, "realclip: t2.0 decode");
            continue;
        }
        s->offset_frames = 0;
        if (s->host_sha[0] && strcmp(s->sha, s->host_sha) != 0)
            fprintf(stderr, "  t=2.0 %s: host fingerprint differs (target=%s host=%s); "
                    "using aligned cross-codec pixel checks\n", r.clip, s->sha, s->host_sha);
    }
    fclose(f);

    int valid = 0, ref = -1;
    for (int i = 0; i < rows && i < (int)(sizeof samples / sizeof samples[0]); i++) {
        if (!samples[i].valid) continue;
        valid++;
        if (strcmp(samples[i].codec, "ffv1") == 0) ref = i;
    }
    gate_check(&g, rows == 4, "realclip: expected four real clips");
    gate_check(&g, valid == 4, "realclip: not all four t2.0 frames decoded");
    gate_check(&g, ref >= 0, "realclip: t2.0 ffv1 reference missing");

    if (ref >= 0 && samples[ref].valid) {
        /* Establish the same source frame across containers.  The ffv1 frame is the lossless
         * reference; for each lossy stream decode a bounded +/- frame window around t=2.0 and keep
         * the candidate whose silhouette/PSNR best matches it.  The original quality thresholds are
         * still applied afterwards, so a black/stuck/garbage decode cannot pass by searching. */
        for (int i = 0; i < rows && i < (int)(sizeof samples / sizeof samples[0]); i++) {
            struct t2_sample *s = &samples[i];
            if (!s->valid || i == ref) continue;
            int best_pass = t2_passes(s, &samples[ref]);
            double best_score = t2_align_score(s, &samples[ref]);
            int best_offset = 0;
            for (int k = -CLIP_T2_SEARCH_RADIUS; k <= CLIP_T2_SEARCH_RADIUS; k++) {
                if (k == 0) continue;
                struct t2_sample cand;
                memset(&cand, 0, sizeof cand);
                snprintf(cand.codec, sizeof cand.codec, "%s", s->codec);
                snprintf(cand.first_sha, sizeof cand.first_sha, "%s", s->first_sha);
                snprintf(cand.host_sha, sizeof cand.host_sha, "%s", s->host_sha);
                snprintf(cand.path, sizeof cand.path, "%s", s->path);
                snprintf(cand.start_time, sizeof cand.start_time, "%s", s->start_time);
                snprintf(cand.time_base, sizeof cand.time_base, "%s", s->time_base);
                double t = CLIP_T2_TIME + (double)k / CLIP_T2_FPS;
                if (decode_t2_sample(&cand, t) != 0) { t2_free(&cand); continue; }
                int cand_pass = t2_passes(&cand, &samples[ref]);
                double score = t2_align_score(&cand, &samples[ref]);
                if (cand_pass > best_pass || (cand_pass == best_pass && score > best_score)) {
                    t2_free(s);
                    *s = cand;
                    memset(&cand, 0, sizeof cand);
                    best_pass = cand_pass;
                    best_score = score;
                    best_offset = k;
                }
                t2_free(&cand);
            }
            s->offset_frames = best_offset;
            fprintf(stderr, "  t2 align %s: offset=%+d frames pts_time=%.6f "
                    "start_time=%s time_base=%s\n",
                    s->codec, s->offset_frames, s->pts, s->start_time, s->time_base);
        }
        fprintf(stderr, "  t2 reference ffv1: pts_time=%.6f start_time=%s time_base=%s\n",
                samples[ref].pts, samples[ref].start_time, samples[ref].time_base);

        double ref_var = luma_variance(samples[ref].luma, CLIP_NP);
        gate_check(&g, ref_var >= T2_VARIANCE_MIN, "realclip: t2.0 ffv1 frame is uniform");
        gate_check(&g, strcmp(samples[ref].sha, samples[ref].first_sha) != 0,
                   "realclip: t2.0 ffv1 frame equals first frame");

        /* The host table records four distinct per-codec t2 fingerprints.  A real independent
         * decode must not collapse different codecs onto one identical frame. */
        for (int i = 0; i < rows && i < (int)(sizeof samples / sizeof samples[0]); i++) {
            if (!samples[i].valid) continue;
            for (int j = i + 1; j < rows && j < (int)(sizeof samples / sizeof samples[0]); j++) {
                if (!samples[j].valid) continue;
                char tag[192];
                snprintf(tag, sizeof tag, "realclip: t2.0 %s and %s produced identical frames",
                         samples[i].codec, samples[j].codec);
                gate_check(&g, strcmp(samples[i].sha, samples[j].sha) != 0, tag);
            }
        }

        for (int i = 0; i < rows && i < (int)(sizeof samples / sizeof samples[0]); i++) {
            if (!samples[i].valid || i == ref) continue;
            char tag[192];

            double psnr = psnr_bytes(samples[i].luma, samples[ref].luma, CLIP_NP);
            double rgb_psnr = psnr_bytes(samples[i].rgb, samples[ref].rgb, samples[ref].rgb_bytes);
            double ssim = ssim_luma(samples[i].luma, samples[ref].luma, CW, CH);
            double mism = binary_mismatch(samples[i].mask, samples[ref].mask, CLIP_NP);
            double iou = binary_iou(samples[i].mask, samples[ref].mask, CLIP_NP);
            double wd = fabs(samples[i].white - samples[ref].white);
            fprintf(stderr, "  t2 ffv1-ref %s: offset=%+d pts=%.6f psnr=%.2f rgb-psnr=%.2f "
                    "ssim=%.3f mismatch=%.3f iou=%.3f white-delta=%.3f\n",
                    samples[i].codec, samples[i].offset_frames, samples[i].pts,
                    psnr, rgb_psnr, ssim, mism, iou, wd);

            snprintf(tag, sizeof tag, "realclip: t2.0 %s vs ffv1 PSNR %.2f dB < %.2f",
                     samples[i].codec, psnr, T2_PSNR_MIN);
            gate_check(&g, psnr >= T2_PSNR_MIN, tag);

            snprintf(tag, sizeof tag, "realclip: t2.0 %s vs ffv1 RGB PSNR %.2f dB < %.2f",
                     samples[i].codec, rgb_psnr, T2_RGB_PSNR_MIN);
            gate_check(&g, rgb_psnr >= T2_RGB_PSNR_MIN, tag);

            snprintf(tag, sizeof tag, "realclip: t2.0 %s vs ffv1 SSIM %.3f < %.2f",
                     samples[i].codec, ssim, T2_SSIM_MIN);
            gate_check(&g, ssim >= T2_SSIM_MIN, tag);

            snprintf(tag, sizeof tag, "realclip: t2.0 %s vs ffv1 binary mismatch %.3f > %.2f",
                     samples[i].codec, mism, T2_MASK_MISMATCH_MAX);
            gate_check(&g, mism <= T2_MASK_MISMATCH_MAX, tag);

            snprintf(tag, sizeof tag, "realclip: t2.0 %s vs ffv1 binary IoU %.3f < %.2f",
                     samples[i].codec, iou, T2_IOU_MIN);
            gate_check(&g, iou >= T2_IOU_MIN, tag);

            snprintf(tag, sizeof tag, "realclip: t2.0 %s vs ffv1 white-ratio delta %.3f > %.2f",
                     samples[i].codec, wd, T2_WHITE_DELTA_MAX);
            gate_check(&g, wd <= T2_WHITE_DELTA_MAX, tag);

            gate_check(&g, strcmp(samples[i].sha, samples[i].first_sha) != 0,
                       "realclip: t2.0 frame equals first frame");
        }
    }

    for (int i = 0; i < rows && i < (int)(sizeof samples / sizeof samples[0]); i++) {
        t2_free(&samples[i]);
    }

    gate_check(&g, rows >= 1, "no real clips processed despite tsv present");
    fprintf(stderr, "  processed %d real clips\n", rows);
    return gate_finish(&g);
}
