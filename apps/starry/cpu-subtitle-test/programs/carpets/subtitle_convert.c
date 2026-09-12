/* subtitle_convert - cross-format timing round-trip (cell 4), CLOSED-FORM.
 *
 * SRT and VTT share one integer-millisecond timing model; the only wire difference is the decimal
 * separator (SRT ',' vs VTT '.') and the WEBVTT header. This cell:
 *   1. authors a synthetic cue set in-code (known ms),
 *   2. serializes it to SRT and to VTT (self-written writer),
 *   3. re-parses both and asserts every timestamp is preserved EXACTLY across SRT->parse and VTT->parse,
 *   4. asserts SRT-serialized text uses ',' and VTT uses '.' at the millisecond boundary,
 *   5. round-trips SRT->(parse)->VTT-string->(parse) and asserts ms identity end to end.
 * If ffmpeg is on PATH (host only), it cross-checks by converting the SRT to VTT and asserting the cue
 * count + first/last timestamps match our parser (structure only) - honest-skip if ffmpeg is absent.
 */
#include "subtitle_common.h"
#include "subtitle_parse.h"

/* the shared synthetic timing model: N cues, known ms, strictly time-ordered (so an external converter that
 * re-sorts by time yields the same cue order for the structural cross-check). */
#define NCUES 6
static const long CUE_START[NCUES] = { 0,     1500,  3000,  60000,  3600000, 3723456 };
static const long CUE_END[NCUES]   = { 1250,  2999,  3500,  61000,  3661000, 3723999 };

/* serialize the cue set to an SRT / VTT buffer using the shared timing model */
static void serialize_srt(char *out, size_t n) {
    size_t off = 0;
    for (int i = 0; i < NCUES; i++) {
        char a[32], b[32];
        format_hms_ms(CUE_START[i], ',', a);
        format_hms_ms(CUE_END[i], ',', b);
        off += (size_t)snprintf(out + off, n - off, "%d\n%s --> %s\nT%d\n\n", i + 1, a, b, i);
    }
}
static void serialize_vtt(char *out, size_t n) {
    size_t off = 0;
    off += (size_t)snprintf(out + off, n - off, "WEBVTT\n\n");
    for (int i = 0; i < NCUES; i++) {
        char a[32], b[32];
        format_hms_ms(CUE_START[i], '.', a);
        format_hms_ms(CUE_END[i], '.', b);
        off += (size_t)snprintf(out + off, n - off, "%s --> %s\nT%d\n\n", a, b, i);
    }
}

int main(void) {
    gate g; gate_init(&g, "SUBTITLE_CONVERT");

    char srt[4096], vtt[4096];
    serialize_srt(srt, sizeof srt);
    serialize_vtt(vtt, sizeof vtt);

    /* separator invariants: SRT uses ',' before ms, VTT uses '.' */
    gate_check(&g, strstr(srt, "00:00:00,000") != NULL, "SRT millisecond boundary uses comma");
    gate_check(&g, strstr(vtt, "00:00:00.000") != NULL, "VTT millisecond boundary uses dot");
    gate_check(&g, strstr(vtt, "WEBVTT") == vtt, "VTT begins with WEBVTT header");

    track ts, tv;
    int ok_srt = parse_srt(srt, &ts) == 0;
    int ok_vtt = parse_vtt(vtt, &tv) == 0;
    gate_check(&g, ok_srt, "serialized SRT re-parses");
    gate_check(&g, ok_vtt, "serialized VTT re-parses");

    if (ok_srt && ok_vtt) {
        gate_check(&g, ts.n == NCUES && tv.n == NCUES, "both formats yield NCUES cues");
        int all_eq = 1;
        for (int i = 0; i < NCUES; i++) {
            if (ts.c[i].start_ms != CUE_START[i] || ts.c[i].end_ms != CUE_END[i]) all_eq = 0;
            if (tv.c[i].start_ms != CUE_START[i] || tv.c[i].end_ms != CUE_END[i]) all_eq = 0;
            if (ts.c[i].start_ms != tv.c[i].start_ms || ts.c[i].end_ms != tv.c[i].end_ms) all_eq = 0;
        }
        gate_check(&g, all_eq, "every cue's start/end preserved exactly across SRT and VTT");

        /* full round-trip: SRT -> parse -> re-serialize to VTT -> parse -> compare ms */
        char vtt2[4096]; size_t off = 0;
        off += (size_t)snprintf(vtt2 + off, sizeof vtt2 - off, "WEBVTT\n\n");
        for (int i = 0; i < ts.n; i++) {
            char a[32], b[32];
            format_hms_ms(ts.c[i].start_ms, '.', a);
            format_hms_ms(ts.c[i].end_ms, '.', b);
            off += (size_t)snprintf(vtt2 + off, sizeof vtt2 - off, "%s --> %s\nR%d\n\n", a, b, i);
        }
        track tr;
        if (parse_vtt(vtt2, &tr) == 0) {
            int rt_ok = (tr.n == ts.n);
            for (int i = 0; i < tr.n && rt_ok; i++)
                if (tr.c[i].start_ms != ts.c[i].start_ms || tr.c[i].end_ms != ts.c[i].end_ms) rt_ok = 0;
            gate_check(&g, rt_ok, "SRT->VTT->parse round-trip preserves all timestamps");
            track_free(&tr);
        } else { gate_check(&g, 0, "round-trip VTT re-parse failed"); }
    }

    /* -------- optional ffmpeg cross-check (host only; honest-skip if absent) -------- */
    if (system("command -v ffmpeg >/dev/null 2>&1") == 0) {
        const char *tmp_srt = "/tmp/cpu-subtitle-convert.srt";
        const char *tmp_vtt = "/tmp/cpu-subtitle-convert.vtt";
        FILE *f = fopen(tmp_srt, "wb");
        int cross_ok = 0;
        if (f) {
            fwrite(srt, 1, strlen(srt), f); fclose(f);
            char cmd[512];
            snprintf(cmd, sizeof cmd, "ffmpeg -y -loglevel error -i %s %s >/dev/null 2>&1", tmp_srt, tmp_vtt);
            if (system(cmd) == 0) {
                size_t len = 0; char *fb = slurp(tmp_vtt, &len);
                if (fb) {
                    track tf;
                    if (parse_vtt(fb, &tf) == 0) {
                        cross_ok = (tf.n == NCUES
                                    && tf.c[0].start_ms == CUE_START[0]
                                    && tf.c[NCUES-1].end_ms == CUE_END[NCUES-1]);
                        if (!cross_ok)
                            fprintf(stderr, "  ffmpeg cross-check: n=%d first=%ld last=%ld\n",
                                    tf.n, tf.c[0].start_ms, tf.c[tf.n-1].end_ms);
                        track_free(&tf);
                    }
                    free(fb);
                }
            }
        }
        gate_check(&g, cross_ok, "ffmpeg srt->vtt matches our cue count + first/last timestamp");
        remove(tmp_srt); remove(tmp_vtt);
    } else {
        fprintf(stderr, "  SKIP: ffmpeg absent - cross-check leg honest-skipped\n");
        gate_check(&g, 1, "ffmpeg absent (honest-skip)");
    }

    track_free(&ts); track_free(&tv);
    return gate_finish(&g);
}
