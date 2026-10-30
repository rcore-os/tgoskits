/* video_realassets - real-media leg (optional, references $ASSET_DIR, honest-skip when absent).
 *
 * The extracted real clips + golden stats live under $ASSET_DIR: video/badapple_clips/<clip> and
 * golden/badapple_clips_firstframe.tsv (clip, codec, sha256_rgb24_firstframe, luma8x8_hex) plus
 * Bit-exact RGB references at t=2.0 are pinned below; the asset TSV predates
 * explicit conversion rounding and describes the x86 SIMD fast path.
 *
 * For each of the four transcodes {h264, hevc, vp9, ffv1}:
 *   - assert codec_name, width==640, height==480, r_frame_rate==30/1 via ffprobe (vs golden),
 *   - decode the first frame to raw rgb24, assert sha256(rgb24) == golden firstframe sha (byte-exact),
 *   - assert the 8x8-bicubic-gray signature of the first frame == golden luma8x8_hex,
 *   - decode the frame at t=2.0 and assert sha256(rgb24) == golden t2 sha (a frame that diverges
 *     across codecs, so it is a per-codec discriminating check),
 *   - assert the reported duration ~= 5.13 s (first-5s transcode).
 *
 * If the golden tsv or the clips directory is absent, every real-clip check honest-skips and the
 * cell prints a SKIP marker with a single satisfied assertion so the synthetic cells still gate.
 */
#include "video_common.h"
#include <sys/stat.h>

#define CW 640
#define CH 480

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

/* Independently generated on native Linux with FFmpeg 6.1.1 and 8.1.2 from
 * media commit 3fbb179fc776f8de607b3c7b1736c9e057e9fc1b, using -ss 2.000000
 * and -vf scale=iw:ih:flags=bicubic+bitexact+accurate_rnd -pix_fmt rgb24.
 * Both CPU-dispatched and scalar paths produce these exact hashes. */
static const struct { const char *clip, *sha; } t2[] = {
    {"badapple_ffv1.mkv", "305728126d6a883a8a74394dd20bbe4b78b6a88a6f40f17e0d5ccd4e1056b415"},
    {"badapple_h264.mp4", "81b368859575358f24192658dade2cb0df2bee019317e810094b63d428e5be00"},
    {"badapple_h265.mp4", "f72e8717c91f6e0b9a8a8ec9bc7045ba79001677478e85c18dd53b2d9cdaff53"},
    {"badapple_vp9.webm", "4d5cb1b8acf9c928b646de80b2e3ee5440773772a3597286293d48f2ab5fe4dd"},
};

int main(void) {
    gate g; gate_init(&g, "VIDEO_REALASSETS");
    const char *AD = asset_dir();
    char ff_tsv[512];
    snprintf(ff_tsv, sizeof ff_tsv, "%s/golden/badapple_clips_firstframe.tsv", AD);

    if (!file_exists(ff_tsv)) {
        fprintf(stderr, "  (assets absent: %s not found - real-clip checks honest-skipped)\n", ff_tsv);
        gate_check(&g, !file_exists(ff_tsv), "asset-skip path");
        printf("VIDEO_REALASSETS SKIP (no assets at %s) ", AD);
        return gate_finish(&g);
    }

    char cmd[256]; snprintf(cmd, sizeof cmd, "mkdir -p %s", TMP); sh(cmd);

    FILE *f = fopen(ff_tsv, "r");
    if (!f) { gate_check(&g, 0, "firstframe tsv open"); return gate_finish(&g); }

    int rows = 0; char line[512];
    while (fgets(line, sizeof line, f)) {
        struct row r;
        if (sscanf(line, "%127s\t%31s\t%79s\t%159s", r.clip, r.codec, r.sha, r.luma) != 4) continue;
        if (strcmp(r.clip, "clip") == 0) continue;   /* header */

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

        /* t=2.0 frame sha, per-codec discriminating (diverges across codecs) */
        const char *want_t2 = NULL;
        for (size_t k = 0; k < sizeof(t2) / sizeof(t2[0]); k++)
            if (strcmp(t2[k].clip, r.clip) == 0) { want_t2 = t2[k].sha; break; }
        if (want_t2) {
            char t2rgb[512]; snprintf(t2rgb, sizeof t2rgb, "%s/t2.rgb", TMP);
            if (ffmpeg_frame_rgb24(clippath, 2.0, "", t2rgb) == 0) {
                char sh2[65];
                gate_check(&g, sha256_file(t2rgb, sh2) == 0 && strcmp(sh2, want_t2) == 0,
                           "realclip: t=2.0 frame rgb24 sha != golden");
            } else gate_check(&g, 0, "realclip: t2.0 decode");
        } else gate_check(&g, 0, "realclip: no bit-exact t2 reference");
        rows++;
    }
    fclose(f);

    gate_check(&g, rows >= 1, "no real clips processed despite tsv present");
    fprintf(stderr, "  processed %d real clips\n", rows);
    return gate_finish(&g);
}
