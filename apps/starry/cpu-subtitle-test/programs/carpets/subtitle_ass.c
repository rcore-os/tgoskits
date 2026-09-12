/* subtitle_ass - Advanced SubStation Alpha parser (cell 2), CLOSED-FORM on synthetic in-code data.
 *
 * The ASS document below is authored HERE with a NON-canonical Events Format ordering (Text is NOT last in
 * the declared order relative to a naive parser's assumption - we place it last per spec but shuffle Layer/
 * Style vs the fixed SSA layout) to prove the parser maps columns by the declared "Format:" line, not by
 * position. Timestamps are H:MM:SS.cc centiseconds -> ms: 0:00:01.50 -> 1500 ms; 1:02:03.99 -> 3723990 ms.
 * Asserts: Dialogue count, Start/End ms, Style name, Layer, style table (Name/Fontname/Fontsize/
 * PrimaryColour &HAABBGGRR), and override-tag stripping for plain-text LENGTH. Fully deterministic.
 */
#include "subtitle_common.h"
#include "subtitle_parse.h"

int main(void) {
    gate g; gate_init(&g, "SUBTITLE_ASS");

    /* Events Format here is: Layer,Start,End,Style,Name,MarginL,MarginR,MarginV,Effect,Text (canonical),
     * plus a SECOND synthetic doc that reorders the columns to Start,End,Layer,Style,...,Text to prove
     * ordering is honored. */
    static const char *ASS_DOC =
        "[Script Info]\n"
        "Title: synthetic\n"
        "ScriptType: v4.00+\n"
        "\n"
        "[V4+ Styles]\n"
        "Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n"
        "Style: Default,Arial,28,&H00AABBCC,&H000000FF,&H00000000,&H80000000,0,0,0,0,100,100,0,0,1,2,0,2,10,10,10,1\n"
        "Style: Title,Times,40,&HFF112233,&H000000FF,&H00000000,&H80000000,-1,0,0,0,100,100,0,0,1,2,0,8,10,10,10,1\n"
        "\n"
        "[Events]\n"
        "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n"
        "Dialogue: 0,0:00:01.50,0:00:02.50,Default,,0,0,0,,Hello world\n"
        "Dialogue: 2,0:00:03.00,0:00:04.00,Title,,0,0,0,,{\\pos(100,200)}Styled\n"
        "Dialogue: 0,1:02:03.99,1:02:05.00,Default,,0,0,0,,Line{\\i1}A{\\i0}B\n";

    track t; ass_doc doc;
    if (parse_ass(ASS_DOC, &t, &doc) == 0) {
        /* Events format ordering honored */
        gate_check(&g, doc.have_events_format, "events Format line parsed");
        gate_check(&g, doc.fmt_layer == 0 && doc.fmt_start == 1 && doc.fmt_end == 2 && doc.fmt_style == 3,
                   "events Format column order Layer,Start,End,Style honored");
        gate_check(&g, doc.fmt_text == 9, "events Format Text column at index 9");
        /* dialogue count + timing */
        gate_check(&g, t.n == 3, "ass dialogue count == 3");
        gate_check(&g, t.c[0].start_ms == 1500, "d1 start 0:00:01.50 == 1500ms");
        gate_check(&g, t.c[0].end_ms == 2500, "d1 end 0:00:02.50 == 2500ms");
        gate_check(&g, t.c[1].start_ms == 3000 && t.c[1].end_ms == 4000, "d2 timing 3000..4000ms");
        gate_check(&g, t.c[2].start_ms == 3723990, "d3 start 1:02:03.99 == 3723990ms");
        gate_check(&g, t.c[2].end_ms == 3725000, "d3 end 1:02:05.00 == 3725000ms");
        gate_check(&g, cue_duration(&t.c[0]) == 1000, "d1 duration 1000ms");
        /* layer + style per dialogue */
        gate_check(&g, t.c[0].layer == 0 && t.c[1].layer == 2 && t.c[2].layer == 0, "dialogue Layer values 0,2,0");
        gate_check(&g, strcmp(t.c[0].style, "Default") == 0, "d1 style Default");
        gate_check(&g, strcmp(t.c[1].style, "Title") == 0, "d2 style Title");
        gate_check(&g, track_starts_monotonic(&t) && track_end_ge_start(&t), "ass monotonic + end>=start");
        /* style table */
        gate_check(&g, doc.nstyles == 2, "style table has 2 styles");
        gate_check(&g, strcmp(doc.styles[0].name, "Default") == 0 && strcmp(doc.styles[0].fontname, "Arial") == 0,
                   "style0 Name=Default Fontname=Arial");
        gate_check(&g, doc.styles[0].fontsize == 28, "style0 Fontsize 28");
        /* PrimaryColour &H00AABBCC -> AA=00 BB=AA GG=BB RR=CC (ASS is &HAABBGGRR) */
        gate_check(&g, doc.styles[0].aa == 0x00 && doc.styles[0].bb == 0xAA && doc.styles[0].gg == 0xBB && doc.styles[0].rr == 0xCC,
                   "style0 PrimaryColour &H00AABBCC decoded AA=00 BB=AA GG=BB RR=CC");
        gate_check(&g, strcmp(doc.styles[1].name, "Title") == 0 && doc.styles[1].fontsize == 40,
                   "style1 Name=Title Fontsize=40");
        gate_check(&g, doc.styles[1].aa == 0xFF && doc.styles[1].bb == 0x11 && doc.styles[1].gg == 0x22 && doc.styles[1].rr == 0x33,
                   "style1 PrimaryColour &HFF112233 decoded AA=FF BB=11 GG=22 RR=33");
        /* override-tag stripping -> plain-text length (not content) */
        gate_check(&g, ass_plain_len(t.c[1].text) == 6, "d2 {\\pos(..)}Styled -> plain len 6");
        gate_check(&g, ass_plain_len(t.c[2].text) == 6, "d3 Line{\\i1}A{\\i0}B -> plain len 6 (LineAB)");
        track_free(&t);
    } else { gate_check(&g, 0, "ASS_DOC parse failed"); }

    /* -------- reordered Events Format proves ordering is honored, not positional -------- */
    static const char *ASS_REORDER =
        "[Events]\n"
        "Format: Start, End, Layer, Style, Text\n"
        "Dialogue: 0:00:10.00,0:00:12.00,5,Neo,body text here\n";
    track t2; ass_doc doc2;
    if (parse_ass(ASS_REORDER, &t2, &doc2) == 0) {
        gate_check(&g, doc2.fmt_start == 0 && doc2.fmt_end == 1 && doc2.fmt_layer == 2 && doc2.fmt_style == 3 && doc2.fmt_text == 4,
                   "reordered Format col map Start,End,Layer,Style,Text");
        gate_check(&g, t2.n == 1, "reordered dialogue count == 1");
        gate_check(&g, t2.c[0].start_ms == 10000 && t2.c[0].end_ms == 12000, "reordered timing 10000..12000ms");
        gate_check(&g, t2.c[0].layer == 5, "reordered Layer read from col 2 == 5");
        gate_check(&g, strcmp(t2.c[0].style, "Neo") == 0, "reordered Style read from col 3 == Neo");
        track_free(&t2);
    } else { gate_check(&g, 0, "ASS_REORDER parse failed"); }

    return gate_finish(&g);
}
