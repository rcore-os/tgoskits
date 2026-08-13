/* video_avsync - audio track + A/V sync carpet (cell 5).
 *
 * The other cells prove the picture decodes correctly. This cell proves (a) the AUDIO track of a
 * container decodes correctly, and (b) the audio and video streams are TIME-ALIGNED - both against a
 * host-decoded golden, and that transcoding preserves both.
 *
 * Deterministic synthetic synced master (analytical golden, no external asset):
 *   video = testsrc  160x120 @ FPS for DUR s  -> exactly FPS*DUR frames, PTS_k = k/FPS.
 *   audio = sine f=TONE_HZ, sr=SR, DUR s      -> exactly SR*DUR samples, tone bin analytically known.
 *   muxed with a LOSSLESS audio codec (flac) so the audio sample count is EXACT (no encoder priming),
 *   giving an exact frame<->sample correspondence: video_dur == audio_dur == DUR, zero drift.
 *
 * Assertions (golden = what host ffmpeg 6.1.1 decodes; on-target re-decodes and must match):
 *   AUDIO TRACK
 *     - demux 0:a -> s16le, sample count == SR*DUR (golden), sample_rate/channels exact,
 *     - decoded audio RMS > 0 and the FFT peak bin == round(TONE_HZ*N/SR) (the sine is intact),
 *     - lossless-audio round-trip: re-decoding the flac audio twice is byte-identical (SHA equal).
 *   A/V SYNC
 *     - video frame count == FPS*DUR (golden); every video PTS_k == k/FPS within a few ms,
 *     - audio duration (samples/SR) == video duration (frames/FPS): NO DRIFT (|Δ| < 1 ms),
 *     - first video PTS == first audio PTS (== container start offset, 0 here) within a frame,
 *     - end-to-end: (last_frame_pts + 1/FPS) == audio_samples/SR (streams end together).
 *   TRANSCODE PRESERVES A/V SYNC
 *     - re-mux the synced master to each {video codec}x{audio codec}: re-demux both streams and assert
 *       they are STILL synced (frame count, sample count within codec tolerance, drift bounded) and
 *       both still decode (video geometry exact, audio tone peak intact). Lossless audio (flac/pcm)
 *       must stay sample-exact; lossy audio (aac) is allowed a small priming drift (< 60 ms).
 *
 * The A/V-offset check is mutation-testable: injecting a deliberate audio delay (see the carpet's
 * mutation note) changes the demuxed sample count / tone position and trips the drift/PTS assertions.
 */
#include "video_common.h"

#define VW 160
#define VH 120
#define FPS 25
#define DUR 2
#define SR 44100
#define TONE_HZ 1000
#define NFRAMES (FPS * DUR)     /* 50  */
#define NSAMPLES (SR * DUR)     /* 88200 */
#define NFFT 8192

static const char *TMP = "/tmp/videoavsync";

/* ---- self-written radix-2 FFT (same convention as the audio carpet) so we can locate the sine ---- */
typedef struct { double re, im; } cpx;
static void fft_run(cpx *a, int n, int sign) {
    for (int i = 1, j = 0; i < n; i++) {
        int bit = n >> 1;
        for (; j & bit; bit >>= 1) j ^= bit;
        j ^= bit;
        if (i < j) { cpx t = a[i]; a[i] = a[j]; a[j] = t; }
    }
    for (int len = 2; len <= n; len <<= 1) {
        double ang = sign * 2.0 * M_PI / len;
        cpx wl = { cos(ang), sin(ang) };
        for (int i = 0; i < n; i += len) {
            cpx w = { 1.0, 0.0 };
            for (int k = 0; k < len/2; k++) {
                cpx u = a[i+k], v;
                v.re = a[i+k+len/2].re*w.re - a[i+k+len/2].im*w.im;
                v.im = a[i+k+len/2].re*w.im + a[i+k+len/2].im*w.re;
                a[i+k].re = u.re+v.re; a[i+k].im = u.im+v.im;
                a[i+k+len/2].re = u.re-v.re; a[i+k+len/2].im = u.im-v.im;
                double nr = w.re*wl.re - w.im*wl.im, ni = w.re*wl.im + w.im*wl.re;
                w.re = nr; w.im = ni;
            }
        }
    }
}
/* FFT peak bin of a mono s16 buffer, NFFT window from `start`. */
static int tone_peak(const int16_t *pcm, long nsamp, long start) {
    if (start + NFFT > nsamp) return -1;
    cpx *a = (cpx *)malloc(sizeof(cpx) * NFFT);
    for (int i = 0; i < NFFT; i++) { a[i].re = pcm[start+i] / 32768.0; a[i].im = 0; }
    fft_run(a, NFFT, -1);
    int best = 1; double bv = -1;
    for (int k = 1; k <= NFFT/2; k++) {
        double m = a[k].re*a[k].re + a[k].im*a[k].im;
        if (m > bv) { bv = m; best = k; }
    }
    free(a);
    return best;
}
static double rms_i16(const int16_t *x, long n) {
    double s = 0; for (long i = 0; i < n; i++) { double v = x[i]/32768.0; s += v*v; }
    return n ? sqrt(s/(double)n) : 0;
}

/* ---- ffprobe helpers ---- */
static long run_capture(const char *cmd, char *buf, long cap) {
    FILE *p = popen(cmd, "r"); if (!p) return -1;
    long n = (long)fread(buf, 1, cap-1, p); pclose(p);
    buf[n < 0 ? 0 : n] = 0;
    while (n > 0 && (buf[n-1]=='\n'||buf[n-1]=='\r'||buf[n-1]==' ')) buf[--n] = 0;
    return n;
}
static long vframe_count(const char *f) {
    char cmd[900], b[64];
    snprintf(cmd, sizeof cmd,
        "ffprobe -v error -select_streams v:0 -count_frames "
        "-show_entries stream=nb_read_frames -of default=nk=1:nw=1 '%s'", f);
    return run_capture(cmd, b, sizeof b) > 0 ? atol(b) : -1;
}
static int vpts(const char *f, double *pts, int maxn) {
    char cmd[900];
    snprintf(cmd, sizeof cmd,
        "ffprobe -v error -select_streams v:0 -show_entries frame=pts_time -of csv=p=0 '%s'", f);
    FILE *p = popen(cmd, "r"); if (!p) return -1;
    int n = 0; char l[64];
    while (n < maxn && fgets(l, sizeof l, p)) if (l[0] && l[0] != '\n') pts[n++] = atof(l);
    pclose(p);
    return n;
}
static int probe_int(const char *f, const char *stream, const char *entry) {
    char cmd[900], b[48];
    snprintf(cmd, sizeof cmd,
        "ffprobe -v error -select_streams %s -show_entries stream=%s -of default=nk=1:nw=1 '%s'",
        stream, entry, f);
    return run_capture(cmd, b, sizeof b) > 0 ? atoi(b) : -1;
}
static double probe_start(const char *f, const char *stream) {
    char cmd[900], b[48];
    snprintf(cmd, sizeof cmd,
        "ffprobe -v error -select_streams %s -show_entries stream=start_time -of default=nk=1:nw=1 '%s'",
        stream, f);
    return run_capture(cmd, b, sizeof b) > 0 ? atof(b) : 0.0;
}

/* Demux the audio track to mono s16le at SR, load into pcm. Returns sample count, -1 err. */
static long demux_audio(const char *f, const char *raw, int16_t **pcm) {
    char cmd[1024];
    snprintf(cmd, sizeof cmd,
        "ffmpeg -v error -y -i '%s' -map 0:a -f s16le -acodec pcm_s16le -ar %d -ac 1 '%s'",
        f, SR, raw);
    if (sh(cmd) != 0) return -1;
    unsigned char *b = NULL; long n = read_file_bytes(raw, &b);
    if (n < 0) return -1;
    *pcm = (int16_t *)b;
    return n / 2;
}

/* Build the deterministic synced master with a lossless (flac) audio track. */
static int make_master(const char *out) {
    char cmd[1024];
    snprintf(cmd, sizeof cmd,
        "ffmpeg -v error -y "
        "-f lavfi -i \"testsrc=size=%dx%d:rate=%d:duration=%d\" "
        "-f lavfi -i \"sine=frequency=%d:sample_rate=%d:duration=%d\" "
        "-c:v libx264 -crf 20 -pix_fmt yuv420p -c:a flac -shortest '%s'",
        VW, VH, FPS, DUR, TONE_HZ, SR, DUR, out);
    return sh(cmd);
}

/* Assert a demuxed clip's audio+video are internally synced to the analytical golden.
 * lossless_audio: sample count must be exactly NSAMPLES; else allow priming drift up to `tol_ms`. */
static void assert_synced(gate *g, const char *file, const char *label,
                          int lossless_audio, double tol_ms) {
    char tag[128];
    int exp_tone = (int)lround((double)TONE_HZ * NFFT / SR);

    /* ---- video side ---- */
    long vf = vframe_count(file);
    snprintf(tag, sizeof tag, "%s: video frame count %ld == golden %d", label, vf, NFRAMES);
    gate_check(g, vf == NFRAMES, tag);

    int w = probe_int(file, "v:0", "width"), h = probe_int(file, "v:0", "height");
    snprintf(tag, sizeof tag, "%s: video geometry %dx%d", label, w, h);
    gate_check(g, w == VW && h == VH, tag);

    double pts[512];
    int np = vpts(file, pts, 512);
    int mono = 1, spaced = 1; double dt = 1.0 / FPS;
    for (int k = 1; k < np; k++) {
        if (!(pts[k] > pts[k-1])) mono = 0;
        if (fabs((pts[k]-pts[k-1]) - dt) > 1e-3) spaced = 0;
    }
    snprintf(tag, sizeof tag, "%s: video PTS monotonic", label);
    gate_check(g, np >= 2 && mono, tag);
    snprintf(tag, sizeof tag, "%s: video PTS_k == k/FPS", label);
    gate_check(g, np >= 2 && spaced, tag);
    /* the container may carry a tiny common start offset (webm ~3ms); require it below one frame */
    snprintf(tag, sizeof tag, "%s: first video PTS %.3f within one frame of container start", label, np?pts[0]:-1);
    gate_check(g, np >= 1 && pts[0] >= 0 && pts[0] < dt, tag);

    /* cross-stream start alignment: video and audio must share the SAME start offset (synced),
     * not merely each be near zero - this is the A/V phase check. */
    double vstart = probe_start(file, "v:0"), astart = probe_start(file, "a:0");
    snprintf(tag, sizeof tag, "%s: video/audio start offsets aligned (v=%.3f a=%.3f)", label, vstart, astart);
    gate_check(g, fabs(vstart - astart) < dt, tag);

    /* ---- audio side ---- */
    int asr = probe_int(file, "a:0", "sample_rate");
    int ach = probe_int(file, "a:0", "channels");
    snprintf(tag, sizeof tag, "%s: audio sample_rate %d == %d", label, asr, SR);
    gate_check(g, asr == SR, tag);
    snprintf(tag, sizeof tag, "%s: audio channels %d == 1", label, ach);
    gate_check(g, ach == 1, tag);

    char raw[512]; snprintf(raw, sizeof raw, "%s/a.pcm", TMP);
    int16_t *pcm = NULL; long ns = demux_audio(file, raw, &pcm);
    if (ns <= 0) { snprintf(tag, sizeof tag, "%s: audio demux", label); gate_check(g, 0, tag); free(pcm); return; }

    if (lossless_audio) {
        snprintf(tag, sizeof tag, "%s: lossless audio sample count %ld == golden %d", label, ns, NSAMPLES);
        gate_check(g, ns == NSAMPLES, tag);
    } else {
        long slack = (long)(tol_ms/1000.0 * SR);
        snprintf(tag, sizeof tag, "%s: audio sample count %ld ~ golden %d (+/-%ldms)", label, ns, NSAMPLES, (long)tol_ms);
        gate_check(g, labs(ns - NSAMPLES) <= slack, tag);
    }

    double rms = rms_i16(pcm, ns);
    snprintf(tag, sizeof tag, "%s: audio RMS > 0 (%.4f)", label, rms);
    gate_check(g, rms > 0.001, tag);
    int pk = tone_peak(pcm, ns, ns/2 - NFFT/2 > 0 ? ns/2 - NFFT/2 : 0);
    snprintf(tag, sizeof tag, "%s: audio tone bin %d == golden %d", label, pk, exp_tone);
    gate_check(g, abs(pk - exp_tone) <= 1, tag);

    /* ---- A/V sync: drift + streams-end-together ---- */
    double vdur = (double)vf / FPS;
    double adur = (double)ns / SR;
    double drift_ms = fabs(vdur - adur) * 1000.0;
    snprintf(tag, sizeof tag, "%s: A/V drift %.2fms < %.1fms (no desync)", label, drift_ms, tol_ms);
    gate_check(g, drift_ms < tol_ms, tag);

    if (np >= 2) {
        /* video span = (last_pts + dt) - first_pts; must equal audio span (samples/SR) - streams
         * cover the same time extent, so no drift accumulates from start to end of the clip. */
        double vspan = (pts[np-1] + dt) - pts[0];
        double span_gap_ms = fabs(vspan - adur) * 1000.0;
        snprintf(tag, sizeof tag, "%s: A/V spans match (v=%.3fs a=%.3fs gap %.2fms)", label, vspan, adur, span_gap_ms);
        gate_check(g, span_gap_ms < tol_ms, tag);
    }
    free(pcm);
}

int main(void) {
    gate g; gate_init(&g, "VIDEO_AVSYNC");
    char cmd[2048]; snprintf(cmd, sizeof cmd, "mkdir -p %s", TMP); sh(cmd);

    char master[512]; snprintf(master, sizeof master, "%s/master.mkv", TMP);
    if (make_master(master) != 0) { gate_check(&g, 0, "avsync: master generate"); return gate_finish(&g); }

    /* golden = the freshly muxed master decoded by host ffmpeg; assert it is exactly synced */
    assert_synced(&g, master, "master", /*lossless_audio*/1, /*tol_ms*/1.0);

    /* lossless audio round-trip determinism: demux twice, byte-identical */
    {
        char r1[512], r2[512]; snprintf(r1, sizeof r1, "%s/rt1.pcm", TMP); snprintf(r2, sizeof r2, "%s/rt2.pcm", TMP);
        int16_t *p1=NULL,*p2=NULL; long n1 = demux_audio(master, r1, &p1); long n2 = demux_audio(master, r2, &p2);
        char h1[65], h2[65]; sha256_file(r1, h1); sha256_file(r2, h2);
        gate_check(&g, n1 == n2 && n1 == NSAMPLES && strcmp(h1, h2) == 0,
                   "avsync: lossless audio demux not deterministic/exact");
        free(p1); free(p2);
    }

    /* transcode matrix: preserve A/V sync + both streams decode.
     * lossless audio (flac/pcm) stays sample-exact (tol 1ms); lossy audio (aac) allows priming (60ms). */
    struct { const char *vc, *ac, *ext; int lossless; double tol; } T[] = {
        {"libx264",    "flac",      "mkv",  1, 1.0 },
        {"libx265",    "flac",      "mkv",  1, 1.0 },
        {"libvpx-vp9", "libvorbis", "webm", 0, 60.0},   /* webm: vp9 + vorbis, vorbis primes ~39ms */
        {"ffv1",       "pcm_s16le", "mkv",  1, 1.0 },
        {"libx264",    "aac",       "mp4",  0, 60.0},   /* mp4: h264 + aac, aac primes */
    };
    int nt = sizeof(T)/sizeof(T[0]);
    for (int i = 0; i < nt; i++) {
        char out[512], label[64];
        snprintf(out, sizeof out, "%s/tx_%d.%s", TMP, i, T[i].ext);
        snprintf(label, sizeof label, "tx[%s/%s/%s]", T[i].vc, T[i].ac, T[i].ext);
        /* x265 is chatty on stderr; silence it so run_all's captured output stays clean */
        const char *vopt = strcmp(T[i].vc, "libx265") == 0
            ? "libx265 -x265-params log-level=none" : T[i].vc;
        snprintf(cmd, sizeof cmd,
            "ffmpeg -v error -y -i '%s' -c:v %s -pix_fmt yuv420p -c:a %s '%s'",
            master, vopt, T[i].ac, out);
        if (sh(cmd) != 0) { char t[96]; snprintf(t, sizeof t, "%s: transcode", label); gate_check(&g, 0, t); continue; }
        assert_synced(&g, out, label, T[i].lossless, T[i].tol);
    }

    return gate_finish(&g);
}
