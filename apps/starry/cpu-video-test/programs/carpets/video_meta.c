/* video_meta - PTS / timing / geometry carpet (cell 3).
 *
 * Generate synthetic clips with an exactly-known geometry, frame rate and duration, then assert the
 * container/stream metadata and the per-frame timestamps that ffmpeg reports back:
 *
 *   - decoded frame count == round(duration * fps)  (deterministic for CFR synthetic clips),
 *   - resolution (w,h) exact, pixel format yuv420p, sample aspect ratio exact,
 *   - r_frame_rate == the requested fps,
 *   - the PTS sequence is strictly monotonic and evenly spaced with dt == 1/fps within a tight eps
 *     (a hard constant-frame-rate timing check, not a smoke probe).
 *
 * Frame timestamps come from `ffprobe -show_entries frame=pts_time`; the frame count from
 * `-count_frames`. We test several {size, fps, duration} points so the timing relation holds across
 * grids, and one non-square-SAR case to exercise the aspect-ratio field. No external asset needed.
 */
#include "video_common.h"

static const char *TMP = "/tmp/videometa";

/* Run a command capturing stdout into buf (up to cap-1 bytes). Returns byte count, -1 on error. */
static long run_capture(const char *cmd, char *buf, long cap) {
    FILE *p = popen(cmd, "r");
    if (!p) return -1;
    long n = (long)fread(buf, 1, cap - 1, p);
    pclose(p);
    buf[n < 0 ? 0 : n] = 0;
    return n;
}

/* One ffprobe stream-entry as a trimmed string. Returns 0 ok. */
static int probe_str(const char *file, const char *entry, char *out, long cap) {
    char cmd[1024];
    snprintf(cmd, sizeof cmd,
        "ffprobe -v error -select_streams v:0 -show_entries stream=%s -of default=nk=1:nw=1 '%s'",
        entry, file);
    long n = run_capture(cmd, out, cap);
    if (n <= 0) return -1;
    while (n > 0 && (out[n-1] == '\n' || out[n-1] == '\r' || out[n-1] == ' ')) out[--n] = 0;
    return 0;
}

static long probe_frame_count(const char *file) {
    char cmd[1024], buf[64];
    snprintf(cmd, sizeof cmd,
        "ffprobe -v error -select_streams v:0 -count_frames "
        "-show_entries stream=nb_read_frames -of default=nk=1:nw=1 '%s'", file);
    if (run_capture(cmd, buf, sizeof buf) <= 0) return -1;
    return atol(buf);
}

/* Read all frame pts_time values into pts[], return count. */
static int probe_pts(const char *file, double *pts, int maxn) {
    char cmd[1024];
    snprintf(cmd, sizeof cmd,
        "ffprobe -v error -select_streams v:0 -show_entries frame=pts_time "
        "-of csv=p=0 '%s'", file);
    FILE *p = popen(cmd, "r");
    if (!p) return -1;
    int n = 0; char line[64];
    while (n < maxn && fgets(line, sizeof line, p)) {
        if (line[0] == '\n' || line[0] == 0) continue;
        pts[n++] = atof(line);
    }
    pclose(p);
    return n;
}

struct clip { int w, h, fps; double dur; const char *sar_filter; const char *sar_expect; };

static void test_clip(gate *g, const struct clip *c, int idx) {
    char file[512], cmd[1200];
    snprintf(file, sizeof file, "%s/m_%d.mp4", TMP, idx);
    /* CFR synthetic clip via testsrc; optional setsar to exercise the aspect field */
    const char *vf = c->sar_filter ? c->sar_filter : "null";
    snprintf(cmd, sizeof cmd,
        "ffmpeg -v error -y -f lavfi -i \"testsrc=size=%dx%d:rate=%d:duration=%.4f\" "
        "-vf \"%s\" -c:v libx264 -crf 20 -pix_fmt yuv420p -x264-params keyint=15 '%s'",
        c->w, c->h, c->fps, c->dur, vf, file);
    if (sh(cmd) != 0) { gate_check(g, 0, "meta: clip generate"); return; }

    char tag[96];

    /* geometry */
    char sw[32], sh_[32], pf[32], sar[32], rfr[32];
    probe_str(file, "width", sw, sizeof sw);
    probe_str(file, "height", sh_, sizeof sh_);
    probe_str(file, "pix_fmt", pf, sizeof pf);
    probe_str(file, "sample_aspect_ratio", sar, sizeof sar);
    probe_str(file, "r_frame_rate", rfr, sizeof rfr);
    snprintf(tag, sizeof tag, "meta[%d]: width", idx);
    gate_check(g, atoi(sw) == c->w, tag);
    snprintf(tag, sizeof tag, "meta[%d]: height", idx);
    gate_check(g, atoi(sh_) == c->h, tag);
    snprintf(tag, sizeof tag, "meta[%d]: pix_fmt yuv420p", idx);
    gate_check(g, strcmp(pf, "yuv420p") == 0, tag);
    char rfr_want[32]; snprintf(rfr_want, sizeof rfr_want, "%d/1", c->fps);
    snprintf(tag, sizeof tag, "meta[%d]: r_frame_rate", idx);
    gate_check(g, strcmp(rfr, rfr_want) == 0, tag);
    if (c->sar_expect) {
        snprintf(tag, sizeof tag, "meta[%d]: sample_aspect_ratio (%s)", idx, sar);
        gate_check(g, strcmp(sar, c->sar_expect) == 0, tag);
    }

    /* frame count == round(dur*fps) */
    long want = (long)llround(c->dur * c->fps);
    long got = probe_frame_count(file);
    snprintf(tag, sizeof tag, "meta[%d]: frame count %ld == dur*fps %ld", idx, got, want);
    gate_check(g, got == want, tag);

    /* PTS monotonic + evenly spaced dt == 1/fps */
    double pts[4096];
    int n = probe_pts(file, pts, 4096);
    snprintf(tag, sizeof tag, "meta[%d]: pts count == frames", idx);
    gate_check(g, n == want, tag);
    double dt = 1.0 / c->fps;
    int mono = 1, spaced = 1;
    for (int k = 1; k < n; k++) {
        if (!(pts[k] > pts[k-1])) mono = 0;
        if (fabs((pts[k] - pts[k-1]) - dt) > 1e-4) spaced = 0;
    }
    snprintf(tag, sizeof tag, "meta[%d]: pts strictly monotonic", idx);
    gate_check(g, n >= 2 && mono, tag);
    snprintf(tag, sizeof tag, "meta[%d]: pts spacing == 1/fps", idx);
    gate_check(g, n >= 2 && spaced, tag);
    snprintf(tag, sizeof tag, "meta[%d]: pts[0] == 0", idx);
    gate_check(g, n >= 1 && fabs(pts[0]) < 1e-6, tag);
}

int main(void) {
    gate g; gate_init(&g, "VIDEO_META");
    char cmd[256]; snprintf(cmd, sizeof cmd, "mkdir -p %s", TMP); sh(cmd);

    struct clip clips[] = {
        {320, 240, 25, 1.0, NULL, NULL},
        {160, 120, 30, 0.5, NULL, NULL},
        {176, 144, 24, 1.0, NULL, NULL},
        {320, 240, 30, 1.0, "setsar=4/3", "4:3"},   /* non-square SAR case (ffprobe prints w:h) */
    };
    int nc = sizeof(clips) / sizeof(clips[0]);
    for (int i = 0; i < nc; i++) test_clip(&g, &clips[i], i);

    return gate_finish(&g);
}
