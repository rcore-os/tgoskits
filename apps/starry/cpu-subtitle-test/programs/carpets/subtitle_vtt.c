/* subtitle_vtt - WebVTT parser (cell 3), CLOSED-FORM on synthetic in-code data.
 *
 * "WEBVTT" header required; NOTE comment blocks skipped; cue timestamps HH:MM:SS.mmm (dot decimal); cue
 * settings (position:/align:) after the "-->" are tolerated; optional cue-identifier line before timings.
 * Timestamps: 00:00:01.000 -> 1000 ms ; 00:01:00.250 -> 60250 ms. Fully deterministic - no asset dependency.
 */
#include "subtitle_common.h"
#include "subtitle_parse.h"

int main(void) {
    gate g; gate_init(&g, "SUBTITLE_VTT");

    static const char *VTT =
        "WEBVTT - synthetic\n"
        "\n"
        "NOTE this is a comment block\n"
        "spanning two lines\n"
        "\n"
        "1\n"
        "00:00:01.000 --> 00:00:02.500 position:50% align:middle\n"
        "AAA\n"
        "\n"
        "00:00:02.500 --> 00:00:04.000\n"
        "BBB line one\n"
        "BBB line two\n"
        "\n"
        "cue-id-3\n"
        "00:01:00.250 --> 00:01:01.000\n"
        "CCC\n";

    track t;
    if (parse_vtt(VTT, &t) == 0) {
        gate_check(&g, t.n == 3, "vtt cue count == 3 (NOTE block skipped)");
        gate_check(&g, t.c[0].start_ms == 1000, "cue1 00:00:01.000 == 1000ms");
        gate_check(&g, t.c[0].end_ms == 2500, "cue1 end 00:00:02.500 == 2500ms");
        gate_check(&g, t.c[1].start_ms == 2500 && t.c[1].end_ms == 4000, "cue2 timing 2500..4000ms");
        gate_check(&g, t.c[2].start_ms == 60250, "cue3 00:01:00.250 == 60250ms");
        gate_check(&g, t.c[2].end_ms == 61000, "cue3 end 00:01:01.000 == 61000ms");
        gate_check(&g, cue_duration(&t.c[0]) == 1500, "cue1 duration 1500ms");
        gate_check(&g, cue_duration(&t.c[2]) == 750, "cue3 duration 750ms");
        gate_check(&g, strcmp(t.c[0].text, "AAA") == 0, "cue1 text AAA (settings not in body)");
        gate_check(&g, strcmp(t.c[1].text, "BBB line one\nBBB line two") == 0, "cue2 multi-line joined");
        gate_check(&g, strcmp(t.c[2].text, "CCC") == 0, "cue3 with identifier line, text CCC");
        gate_check(&g, track_starts_monotonic(&t) && track_end_ge_start(&t), "vtt monotonic + end>=start");
        track_free(&t);
    } else { gate_check(&g, 0, "VTT parse failed"); }

    /* -------- header enforcement: a non-WEBVTT buffer must fail to parse -------- */
    static const char *NOT_VTT = "NOTWEBVTT\n00:00:00.000 --> 00:00:01.000\nX\n";
    track tf;
    int rc = parse_vtt(NOT_VTT, &tf);
    gate_check(&g, rc != 0, "missing WEBVTT header rejected");
    if (rc == 0) track_free(&tf);

    /* -------- VTT permits MM:SS.mmm (no hours) -------- */
    static const char *VTT_SHORT =
        "WEBVTT\n\n05:30.500 --> 05:31.000\nshort\n";
    track ts;
    if (parse_vtt(VTT_SHORT, &ts) == 0) {
        gate_check(&g, ts.n == 1, "short-form cue count == 1");
        gate_check(&g, ts.c[0].start_ms == 330500, "05:30.500 == 330500ms");
        gate_check(&g, ts.c[0].end_ms == 331000, "05:31.000 == 331000ms");
        track_free(&ts);
    } else { gate_check(&g, 0, "VTT_SHORT parse failed"); }

    return gate_finish(&g);
}
