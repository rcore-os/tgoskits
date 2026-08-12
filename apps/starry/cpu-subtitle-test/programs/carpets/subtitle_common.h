/* subtitle_common.h - shared primitives for the cpu-subtitle-test carpet (a "pyte for subtitles").
 *
 * Each cell drives a real subtitle pipeline the carpet implements itself - a SubRip (.srt) parser, an
 * Advanced SubStation Alpha (.ass) parser, a WebVTT (.vtt) parser, a cross-format timing converter - and
 * asserts the output against a golden that is either FULLY DETERMINISTIC (synthetic cues authored in-code,
 * so every timestamp is known: 00:00:01,000 -> 1000 ms; 01:02:03,456 -> 3723456 ms) or a STRUCTURAL golden
 * computed host-side from the real bilibili-sourced files (cue/dialogue count, first/last timestamp,
 * monotonic ordering, valid UTF-8) - never the dialogue text. "Subtitle parsed" alone is NOT a test here.
 *
 * The .srt/.ass/.vtt parsers and the timestamp arithmetic are self-written: these are small, well-specified
 * text formats, so a clean parser is not "reinventing a heavy lib" - and writing them ourselves is exactly
 * what lets a synthetic cue set round-trip SRT<->VTT with millisecond-exact timing.
 *
 * IMPORTANT: the real .srt/.ass files are copyrighted (bilibili-sourced). The carpet asserts STRUCTURE
 * (counts, timestamps, index order, encoding), never the literal dialogue - no cell reproduces or echoes
 * the real dialogue text.
 */
#ifndef SUBTITLE_COMMON_H
#define SUBTITLE_COMMON_H

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <ctype.h>

/* -------- asset locations -------- */
static const char *sub_dir(void) {
    const char *d = getenv("SUBTITLE_DIR");
    if (d && *d) return d;
    d = getenv("ASSET_DIR");
    if (d && *d) return d;
    return "/opt/cpu-subtitle-test/assets";
}
static const char *sub_path(char *buf, size_t n, const char *name) {
    snprintf(buf, n, "%s/%s", sub_dir(), name);
    return buf;
}

/* self-written strdup (avoids depending on POSIX feature-test macros under -std=c11) */
static char *dupstr(const char *s) {
    size_t n = strlen(s) + 1;
    char *p = (char *)malloc(n);
    if (p) memcpy(p, s, n);
    return p;
}

/* Read an entire file into a NUL-terminated heap buffer. Returns NULL on failure; sets *len (excl. NUL). */
static char *slurp(const char *path, size_t *len) {
    FILE *f = fopen(path, "rb");
    if (!f) return NULL;
    fseek(f, 0, SEEK_END); long sz = ftell(f); fseek(f, 0, SEEK_SET);
    if (sz < 0) { fclose(f); return NULL; }
    char *buf = (char *)malloc((size_t)sz + 1);
    if (!buf) { fclose(f); return NULL; }
    size_t rd = fread(buf, 1, (size_t)sz, f);
    fclose(f);
    buf[rd] = '\0';
    if (len) *len = rd;
    return buf;
}

/* -------- three-gate marker (identical semantics to the model/image/font carpets) -------- */
typedef struct { int pass, total, fail; const char *name; } gate;
static void gate_init(gate *g, const char *name) { g->pass = g->total = g->fail = 0; g->name = name; }
static void gate_check(gate *g, int cond, const char *msg) {
    g->total++;
    if (cond) g->pass++;
    else { g->fail++; fprintf(stderr, "  FAIL: %s\n", msg); }
}
static int gate_finish(gate *g) {
    if (g->fail == 0 && g->total == g->pass && g->total > 0) {
        printf("%s OK %d\n", g->name, g->total);
        return 0;
    }
    printf("%s FAILED pass=%d total=%d fail=%d\n", g->name, g->pass, g->total, g->fail);
    return 1;
}

/* ======================================================================== */
/*                     subtitle model: cue + track                           */
/* ======================================================================== */

/* A cue is a timed text span. start_ms/end_ms are integer milliseconds (the common timing model across
 * SRT, VTT and ASS after centisecond -> ms scaling), so round-tripping is exact. index is the 1-based-ish
 * running index for SRT (the parser records the file's own index value). text is the plain joined text
 * (multi-line cues joined with '\n'); the carpet asserts LENGTHS and STRUCTURE, not literal content. */
typedef struct {
    long index;         /* SRT explicit index; -1 if the format has none */
    long start_ms;
    long end_ms;
    char *text;         /* heap; joined multi-line body (may be empty string) */
    char *style;        /* ASS style name; NULL for SRT/VTT */
    int  layer;         /* ASS layer; 0 for SRT/VTT */
} cue;

typedef struct { cue *c; int n, cap; } track;

static void track_init(track *t) { memset(t, 0, sizeof *t); }
/* Append a zeroed cue. Returns NULL on allocation failure (the old block is preserved, not leaked or
 * dereferenced) so parsers can propagate OOM as an ordinary parse failure instead of crashing. */
static cue *track_push(track *t) {
    if (t->n == t->cap) {
        int cap = t->cap ? t->cap * 2 : 64;
        cue *grown = (cue *)realloc(t->c, (size_t)cap * sizeof(cue));
        if (!grown) return NULL;
        t->c = grown;
        t->cap = cap;
    }
    cue *c = &t->c[t->n++];
    memset(c, 0, sizeof *c);
    c->index = -1;
    return c;
}
static void track_free(track *t) {
    for (int i = 0; i < t->n; i++) { free(t->c[i].text); free(t->c[i].style); }
    free(t->c);
    memset(t, 0, sizeof *t);
}

/* duration helper */
static long cue_duration(const cue *c) { return c->end_ms - c->start_ms; }

/* structural checks reused across formats */
static int track_starts_monotonic(const track *t) {
    for (int i = 0; i + 1 < t->n; i++)
        if (t->c[i].start_ms > t->c[i + 1].start_ms) return 0;
    return 1;
}
static int track_end_ge_start(const track *t) {
    for (int i = 0; i < t->n; i++)
        if (t->c[i].end_ms < t->c[i].start_ms) return 0;
    return 1;
}
/* every timestamp within [0, upper] inclusive */
static int track_within_bounds(const track *t, long upper) {
    for (int i = 0; i < t->n; i++) {
        if (t->c[i].start_ms < 0 || t->c[i].end_ms < 0) return 0;
        if (t->c[i].start_ms > upper || t->c[i].end_ms > upper) return 0;
    }
    return 1;
}

/* -------- UTF-8 well-formedness validator (RFC 3629) -------- */
/* Returns 1 if the whole buffer [0,len) is valid UTF-8, else 0. Rejects overlong encodings, surrogates,
 * and > U+10FFFF. */
static int utf8_valid(const unsigned char *s, size_t len) {
    size_t i = 0;
    while (i < len) {
        unsigned char b = s[i];
        if (b < 0x80) { i++; continue; }
        int extra; unsigned int cp; unsigned int lo, hi;
        if ((b & 0xE0) == 0xC0) { extra = 1; cp = b & 0x1F; lo = 0x80;    hi = 0x7FF; }
        else if ((b & 0xF0) == 0xE0) { extra = 2; cp = b & 0x0F; lo = 0x800;   hi = 0xFFFF; }
        else if ((b & 0xF8) == 0xF0) { extra = 3; cp = b & 0x07; lo = 0x10000; hi = 0x10FFFF; }
        else return 0; /* stray continuation or 5/6-byte lead */
        if (i + (size_t)extra >= len + 0) { /* need extra more bytes */ }
        if (i + (size_t)extra >= len) return 0;
        for (int k = 1; k <= extra; k++) {
            unsigned char cb = s[i + k];
            if ((cb & 0xC0) != 0x80) return 0;
            cp = (cp << 6) | (cb & 0x3F);
        }
        if (cp < lo || cp > hi) return 0;              /* overlong or out of range */
        if (cp >= 0xD800 && cp <= 0xDFFF) return 0;    /* UTF-16 surrogate */
        i += (size_t)extra + 1;
    }
    return 1;
}

/* -------- millisecond timestamp arithmetic (self-written, shared by all format parsers) -------- */

/* SRT / VTT hours:minutes:seconds with a fractional millisecond field.
 * sep is the decimal separator: ',' for SRT, '.' for VTT. Parses "HH:MM:SS<sep>mmm" (VTT also permits
 * "MM:SS<sep>mmm"). Returns 0 and sets *out_ms on success, -1 on malformed input; *adv gets the count of
 * characters consumed. */
static int parse_hms_ms(const char *p, char sep, long *out_ms, int *adv) {
    const char *start = p;
    long a = 0, b = 0, c = 0, ms = 0;
    int fields[3]; int nf = 0;
    long cur = 0; int digits = 0;
    /* read up to three colon-separated integer fields, then the fractional part */
    while (*p) {
        if (*p >= '0' && *p <= '9') { cur = cur * 10 + (*p - '0'); digits++; p++; }
        else if (*p == ':') { if (nf >= 3) return -1; fields[nf++] = (int)cur; cur = 0; digits = 0; p++; }
        else break;
    }
    if (digits == 0) return -1;
    /* the last integer field before the separator */
    if (nf == 0) { c = cur; }
    else if (nf == 1) { b = fields[0]; c = cur; }
    else { a = fields[0]; b = fields[1]; c = cur; }
    (void)fields;
    if (*p != sep) return -1;
    p++;
    long msc = 0; int md = 0;
    while (*p >= '0' && *p <= '9') { msc = msc * 10 + (*p - '0'); md++; p++; }
    if (md == 0) return -1;
    /* normalize fractional to milliseconds: 3 digits = ms, 2 digits (ASS style) handled elsewhere */
    if (md == 3) ms = msc;
    else if (md == 2) ms = msc * 10;
    else if (md == 1) ms = msc * 100;
    else { /* more than 3: truncate to ms */
        while (md > 3) { msc /= 10; md--; }
        ms = msc;
    }
    *out_ms = ((a * 60 + b) * 60 + c) * 1000 + ms;
    if (adv) *adv = (int)(p - start);
    return 0;
}

/* ASS H:MM:SS.cc (centisecond, exactly 2 fractional digits). Returns 0 / -1. */
static int parse_ass_time(const char *p, long *out_ms) {
    long h = 0, m = 0, s = 0, cc = 0; int d;
    d = 0; long v = 0;
    while (*p >= '0' && *p <= '9') { v = v * 10 + (*p - '0'); d++; p++; }
    if (d == 0 || *p != ':') return -1;
    h = v; p++;
    v = 0; d = 0;
    while (*p >= '0' && *p <= '9') { v = v * 10 + (*p - '0'); d++; p++; }
    if (d == 0 || *p != ':') return -1;
    m = v; p++;
    v = 0; d = 0;
    while (*p >= '0' && *p <= '9') { v = v * 10 + (*p - '0'); d++; p++; }
    if (d == 0 || *p != '.') return -1;
    s = v; p++;
    v = 0; d = 0;
    while (*p >= '0' && *p <= '9') { v = v * 10 + (*p - '0'); d++; p++; }
    if (d != 2) return -1;
    cc = v;
    *out_ms = ((h * 60 + m) * 60 + s) * 1000 + cc * 10;
    return 0;
}

/* Format a ms value back into SRT "HH:MM:SS,mmm" or VTT "HH:MM:SS.mmm". sep = ',' or '.'. */
static void format_hms_ms(long ms, char sep, char out[32]) {
    long h = ms / 3600000; ms %= 3600000;
    long m = ms / 60000;   ms %= 60000;
    long s = ms / 1000;    long f = ms % 1000;
    snprintf(out, 32, "%02ld:%02ld:%02ld%c%03ld", h, m, s, sep, f);
}

#endif /* SUBTITLE_COMMON_H */
