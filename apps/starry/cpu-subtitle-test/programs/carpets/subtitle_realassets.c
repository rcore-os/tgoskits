/* subtitle_realassets - parse the real bilibili-sourced .srt/.ass files (cell 5), STRUCTURE-ONLY goldens.
 *
 * IMPORTANT: the real files are copyrighted. This cell asserts ONLY structural properties - cue/dialogue
 * count, first/last timestamp (ms), monotonic ordering, all timestamps within [0, media_duration], valid
 * UTF-8, style/layer uniformity, and no malformed cues. It never stores, echoes or asserts the dialogue
 * text. The structural goldens were computed host-side from the exact files (counts + timing bounds).
 * The prebuild always stages both files, so a missing asset here is a hard FAIL, not a skip.
 *
 * Structural goldens (computed from the shipped files):
 *   tashouheng.srt : 48 cues, index sequence 0..47, first start 27400 ms, last end 207080 ms,
 *                    monotonic, end>=start, UTF-8 valid.
 *   badapple.ass   : 54 Dialogue lines, all Layer 0 / Style "Default", first start 0 ms, last end 210170 ms,
 *                    monotonic, end>=start, UTF-8 valid; style table Default/Arial/20/&H00FFFFFF.
 */
#include "subtitle_common.h"
#include "subtitle_parse.h"

/* structural goldens (NOT text) */
#define SRT_CUES        48
#define SRT_FIRST_IDX   0
#define SRT_LAST_IDX    47
#define SRT_FIRST_START 27400L
#define SRT_LAST_END    207080L
#define SRT_MEDIA_MS    210000L   /* generous media-duration upper bound; every ts must fall within */

#define ASS_DIALOGUES   54
#define ASS_FIRST_START 0L
#define ASS_LAST_END    210170L
#define ASS_MEDIA_MS    212000L

int main(void) {
    gate g; gate_init(&g, "SUBTITLE_REALASSETS");
    char path[512];

    /* ---------------- real SRT: tashouheng.srt ---------------- */
    sub_path(path, sizeof path, "tashouheng.srt");
    size_t slen = 0; char *sbuf = slurp(path, &slen);
    /* Both assets are always staged by prebuild; each one is independently required. A missing SRT must
     * fail this cell on its own - "the ASS happens to be present" must not silence the SRT coverage. */
    gate_check(&g, sbuf != NULL, "tashouheng.srt staged by prebuild (asset present)");
    if (sbuf) {
        gate_check(&g, utf8_valid((const unsigned char *)sbuf, slen), "srt file is well-formed UTF-8");
        track t;
        if (parse_srt(sbuf, &t) == 0) {
            gate_check(&g, t.n == SRT_CUES, "srt cue count == 48");
            gate_check(&g, t.n > 0 && t.c[0].index == SRT_FIRST_IDX, "srt first index == 0");
            gate_check(&g, t.n > 0 && t.c[t.n-1].index == SRT_LAST_IDX, "srt last index == 47");
            /* index sequence contiguous from first */
            int seq_ok = (t.n > 0);
            for (int i = 0; i < t.n; i++) if (t.c[i].index != SRT_FIRST_IDX + i) seq_ok = 0;
            gate_check(&g, seq_ok, "srt index sequence contiguous 0..47");
            gate_check(&g, t.n > 0 && t.c[0].start_ms == SRT_FIRST_START, "srt first start == 27400ms");
            gate_check(&g, t.n > 0 && t.c[t.n-1].end_ms == SRT_LAST_END, "srt last end == 207080ms");
            gate_check(&g, track_starts_monotonic(&t), "srt starts monotonic non-decreasing");
            gate_check(&g, track_end_ge_start(&t), "srt every end >= start (no negative duration)");
            gate_check(&g, track_within_bounds(&t, SRT_MEDIA_MS), "srt all timestamps within [0, media]");
            track_free(&t);
        } else { gate_check(&g, 0, "srt parse failed (malformed cue)"); }
        free(sbuf);
    } else {
        fprintf(stderr, "  FAIL: tashouheng.srt absent under %s - submodule/asset staging is broken\n", sub_dir());
    }

    /* ---------------- real ASS: badapple.ass ---------------- */
    sub_path(path, sizeof path, "badapple.ass");
    size_t alen = 0; char *abuf = slurp(path, &alen);
    /* Likewise the ASS asset is independently required: a missing ASS fails the cell even when the SRT
     * parsed cleanly. Never substitute "at least one asset exists" for full per-format coverage. */
    gate_check(&g, abuf != NULL, "badapple.ass staged by prebuild (asset present)");
    if (abuf) {
        gate_check(&g, utf8_valid((const unsigned char *)abuf, alen), "ass file is well-formed UTF-8");
        track t; ass_doc doc;
        if (parse_ass(abuf, &t, &doc) == 0) {
            gate_check(&g, t.n == ASS_DIALOGUES, "ass dialogue count == 54");
            gate_check(&g, t.n > 0 && t.c[0].start_ms == ASS_FIRST_START, "ass first start == 0ms");
            gate_check(&g, t.n > 0 && t.c[t.n-1].end_ms == ASS_LAST_END, "ass last end == 210170ms");
            gate_check(&g, track_starts_monotonic(&t), "ass starts monotonic non-decreasing");
            gate_check(&g, track_end_ge_start(&t), "ass every end >= start");
            gate_check(&g, track_within_bounds(&t, ASS_MEDIA_MS), "ass all timestamps within [0, media]");
            /* uniformity: all Layer 0, all Style Default (structure, not text) */
            int all_layer0 = 1, all_default = 1;
            for (int i = 0; i < t.n; i++) {
                if (t.c[i].layer != 0) all_layer0 = 0;
                if (!t.c[i].style || strcmp(t.c[i].style, "Default") != 0) all_default = 0;
            }
            gate_check(&g, all_layer0, "ass all dialogues Layer 0");
            gate_check(&g, all_default, "ass all dialogues Style Default");
            /* style table */
            gate_check(&g, doc.nstyles == 1, "ass style table has 1 style");
            gate_check(&g, doc.nstyles > 0 && strcmp(doc.styles[0].name, "Default") == 0, "ass style name Default");
            gate_check(&g, doc.nstyles > 0 && strcmp(doc.styles[0].fontname, "Arial") == 0, "ass style Fontname Arial");
            gate_check(&g, doc.nstyles > 0 && doc.styles[0].fontsize == 20, "ass style Fontsize 20");
            /* PrimaryColour &H00FFFFFF -> AA=00 BB=FF GG=FF RR=FF */
            gate_check(&g, doc.nstyles > 0 && doc.styles[0].aa == 0x00 && doc.styles[0].bb == 0xFF
                           && doc.styles[0].gg == 0xFF && doc.styles[0].rr == 0xFF,
                       "ass PrimaryColour &H00FFFFFF (white, opaque)");
            track_free(&t);
        } else { gate_check(&g, 0, "ass parse failed (malformed dialogue)"); }
        free(abuf);
    } else {
        fprintf(stderr, "  FAIL: badapple.ass absent under %s - submodule/asset staging is broken\n", sub_dir());
    }

    return gate_finish(&g);
}
