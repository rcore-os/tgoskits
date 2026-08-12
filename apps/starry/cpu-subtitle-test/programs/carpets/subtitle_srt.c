/* subtitle_srt - SubRip parser (cell 1), CLOSED-FORM on synthetic in-code data.
 *
 * The SRT strings below are authored HERE, so every timestamp is known analytically:
 *   00:00:01,000 -> 1000 ms ; 00:00:02,500 -> 2500 ms ; 01:02:03,456 -> 3723456 ms.
 * Asserts: cue count, each cue's start/end parsed to exact milliseconds, monotonic non-overlapping
 * ordering, index sequence 1..N, multi-line body joined with '\n', duration = end-start, and the edge
 * cases: BOM, CRLF vs LF, trailing blank lines, empty text. Fully deterministic - no asset dependency.
 */
#include "subtitle_common.h"
#include "subtitle_parse.h"

int main(void) {
    gate g; gate_init(&g, "SUBTITLE_SRT");

    /* -------- canonical 3-cue SRT (LF), indices 1..3, one multi-line body -------- */
    static const char *SRT_LF =
        "1\n"
        "00:00:01,000 --> 00:00:02,500\n"
        "AAA\n"
        "\n"
        "2\n"
        "00:00:02,500 --> 00:00:04,000\n"
        "BBB line one\n"
        "BBB line two\n"
        "\n"
        "3\n"
        "01:02:03,456 --> 01:02:04,000\n"
        "CCC\n";

    track t;
    if (parse_srt(SRT_LF, &t) == 0) {
        gate_check(&g, t.n == 3, "srt cue count == 3");
        /* exact millisecond parse */
        gate_check(&g, t.c[0].start_ms == 1000, "cue1 start 00:00:01,000 == 1000ms");
        gate_check(&g, t.c[0].end_ms == 2500, "cue1 end 00:00:02,500 == 2500ms");
        gate_check(&g, t.c[1].start_ms == 2500, "cue2 start == 2500ms");
        gate_check(&g, t.c[1].end_ms == 4000, "cue2 end == 4000ms");
        gate_check(&g, t.c[2].start_ms == 3723456, "cue3 start 01:02:03,456 == 3723456ms");
        gate_check(&g, t.c[2].end_ms == 3724000, "cue3 end 01:02:04,000 == 3724000ms");
        /* index sequence 1..N */
        gate_check(&g, t.c[0].index == 1 && t.c[1].index == 2 && t.c[2].index == 3, "index sequence 1..3");
        /* monotonic non-overlapping (each start >= previous end) */
        gate_check(&g, t.c[1].start_ms >= t.c[0].end_ms, "cue2 starts at/after cue1 end (non-overlap)");
        gate_check(&g, t.c[2].start_ms >= t.c[1].end_ms, "cue3 starts at/after cue2 end (non-overlap)");
        gate_check(&g, track_starts_monotonic(&t), "starts monotonic non-decreasing");
        /* duration = end - start */
        gate_check(&g, cue_duration(&t.c[0]) == 1500, "cue1 duration 1500ms");
        gate_check(&g, cue_duration(&t.c[1]) == 1500, "cue2 duration 1500ms");
        gate_check(&g, cue_duration(&t.c[2]) == 544, "cue3 duration 544ms");
        /* text bodies */
        gate_check(&g, strcmp(t.c[0].text, "AAA") == 0, "cue1 text AAA");
        gate_check(&g, strcmp(t.c[1].text, "BBB line one\nBBB line two") == 0, "cue2 multi-line joined with \\n");
        gate_check(&g, strcmp(t.c[2].text, "CCC") == 0, "cue3 text CCC");
        track_free(&t);
    } else { gate_check(&g, 0, "SRT_LF parse failed"); }

    /* -------- CRLF variant of the same content parses identically -------- */
    static const char *SRT_CRLF =
        "1\r\n"
        "00:00:01,000 --> 00:00:02,500\r\n"
        "AAA\r\n"
        "\r\n"
        "2\r\n"
        "00:00:02,500 --> 00:00:04,000\r\n"
        "BBB line one\r\n"
        "BBB line two\r\n"
        "\r\n";
    track tc;
    if (parse_srt(SRT_CRLF, &tc) == 0) {
        gate_check(&g, tc.n == 2, "crlf cue count == 2");
        gate_check(&g, tc.c[0].start_ms == 1000 && tc.c[0].end_ms == 2500, "crlf cue1 timing exact");
        gate_check(&g, strcmp(tc.c[0].text, "AAA") == 0, "crlf cue1 text has no stray CR");
        gate_check(&g, strcmp(tc.c[1].text, "BBB line one\nBBB line two") == 0, "crlf multi-line joined, CR stripped");
        track_free(&tc);
    } else { gate_check(&g, 0, "SRT_CRLF parse failed"); }

    /* -------- BOM prefix is tolerated -------- */
    static const char *SRT_BOM =
        "\xEF\xBB\xBF" "1\n00:00:00,000 --> 00:00:01,000\nX\n";
    track tb;
    if (parse_srt(SRT_BOM, &tb) == 0) {
        gate_check(&g, tb.n == 1, "bom cue count == 1");
        gate_check(&g, tb.c[0].index == 1, "bom index parsed past BOM == 1");
        gate_check(&g, tb.c[0].start_ms == 0 && tb.c[0].end_ms == 1000, "bom cue timing exact");
        track_free(&tb);
    } else { gate_check(&g, 0, "SRT_BOM parse failed"); }

    /* -------- trailing blank lines + empty-text cue -------- */
    static const char *SRT_EDGE =
        "1\n00:00:05,000 --> 00:00:06,000\n\n"   /* empty text cue (blank line right after timing) */
        "2\n00:00:06,000 --> 00:00:07,000\nHello\n\n\n\n"; /* trailing blank lines */
    track te;
    if (parse_srt(SRT_EDGE, &te) == 0) {
        gate_check(&g, te.n == 2, "edge cue count == 2 (empty-text + trailing blanks)");
        gate_check(&g, te.c[0].text[0] == '\0', "empty-text cue has zero-length body");
        gate_check(&g, cue_duration(&te.c[0]) == 1000, "empty-text cue duration 1000ms");
        gate_check(&g, strcmp(te.c[1].text, "Hello") == 0, "second cue text Hello");
        track_free(&te);
    } else { gate_check(&g, 0, "SRT_EDGE parse failed"); }

    /* -------- a larger monotonic sequence: 10 cues index 1..10, 1s apart -------- */
    {
        char big[4096]; size_t off = 0;
        for (int i = 1; i <= 10; i++) {
            long s = (long)i * 1000, e = s + 500;
            char a[32], b[32]; format_hms_ms(s, ',', a); format_hms_ms(e, ',', b);
            off += (size_t)snprintf(big + off, sizeof big - off, "%d\n%s --> %s\nT%d\n\n", i, a, b, i);
        }
        track tt;
        if (parse_srt(big, &tt) == 0) {
            gate_check(&g, tt.n == 10, "big cue count == 10");
            int seq_ok = 1, mono_ok = 1;
            for (int i = 0; i < tt.n; i++) {
                if (tt.c[i].index != i + 1) seq_ok = 0;
                if (tt.c[i].start_ms != (long)(i + 1) * 1000) mono_ok = 0;
                if (cue_duration(&tt.c[i]) != 500) mono_ok = 0;
            }
            gate_check(&g, seq_ok, "big index sequence 1..10");
            gate_check(&g, mono_ok, "big timing exact + duration 500ms each");
            gate_check(&g, track_starts_monotonic(&tt) && track_end_ge_start(&tt), "big monotonic + end>=start");
            track_free(&tt);
        } else { gate_check(&g, 0, "SRT big parse failed"); }
    }

    return gate_finish(&g);
}
