/* subtitle_parse.h - self-written SubRip / WebVTT / Advanced SubStation Alpha parsers.
 *
 * All three are small, well-specified text formats, so a clean parser is not "reinventing a heavy lib".
 * Each parser fills a `track` of `cue`s with integer-millisecond timing (the shared timing model), so a
 * synthetic cue set round-trips SRT<->VTT millisecond-exact and the ASS centisecond field scales to ms.
 *
 * These parsers operate on an in-memory NUL-terminated buffer (from slurp() or an in-code synthetic string)
 * and are tolerant of BOM, CRLF vs LF, trailing blank lines and empty cue text - the edge cases the carpet
 * asserts. They never interpret or transform the dialogue content beyond joining multi-line bodies and
 * (for ASS) stripping override tags to measure plain-text LENGTH.
 */
#ifndef SUBTITLE_PARSE_H
#define SUBTITLE_PARSE_H

#include "subtitle_common.h"

/* Skip a leading UTF-8 BOM (EF BB BF) if present; returns the adjusted pointer. */
static const char *skip_bom(const char *p) {
    const unsigned char *u = (const unsigned char *)p;
    if (u[0] == 0xEF && u[1] == 0xBB && u[2] == 0xBF) return p + 3;
    return p;
}

/* Copy [s,e) into a fresh heap string, translating CRLF/CR internal newlines to '\n', trimming a single
 * trailing '\r' per line. Used to normalize multi-line cue bodies. */
static char *dup_range(const char *s, const char *e) {
    size_t n = (size_t)(e - s);
    char *out = (char *)malloc(n + 1);
    size_t j = 0;
    for (size_t i = 0; i < n; i++) {
        if (s[i] == '\r') continue;   /* drop CR; keep LF */
        out[j++] = s[i];
    }
    out[j] = '\0';
    return out;
}

/* Advance p past the current line, returning pointer to the start of the next line. Sets *line_end to the
 * end of the current line's content (before CR/LF). */
static const char *next_line(const char *p, const char **line_end) {
    const char *q = p;
    while (*q && *q != '\n' && *q != '\r') q++;
    *line_end = q;
    if (*q == '\r') q++;
    if (*q == '\n') q++;
    return q;
}

static int line_is_blank(const char *s, const char *e) {
    for (const char *p = s; p < e; p++) if (*p != ' ' && *p != '\t' && *p != '\r') return 0;
    return 1;
}

/* ------------------------------ SubRip (.srt) ------------------------------ */
/* Grammar per cue block: an integer index line, a "start --> end" timing line, then one or more text lines,
 * terminated by a blank line or EOF. Tolerant of BOM, CRLF, trailing blank lines, empty text. */
static int parse_srt(const char *buf, track *t) {
    const char *p = skip_bom(buf);
    track_init(t);
    while (*p) {
        const char *le;
        /* skip blank separators */
        const char *nl = next_line(p, &le);
        if (line_is_blank(p, le)) { p = nl; continue; }
        /* index line */
        char idxbuf[32]; size_t il = (size_t)(le - p); if (il >= sizeof idxbuf) il = sizeof idxbuf - 1;
        memcpy(idxbuf, p, il); idxbuf[il] = '\0';
        char *endp; long idx = strtol(idxbuf, &endp, 10);
        if (endp == idxbuf) return -1; /* not an integer index */
        p = nl;
        /* timing line */
        nl = next_line(p, &le);
        const char *q = p;
        long start_ms, end_ms; int adv;
        while (q < le && (*q == ' ' || *q == '\t')) q++;
        if (parse_hms_ms(q, ',', &start_ms, &adv) != 0) return -1;
        q += adv;
        while (q < le && (*q == ' ' || *q == '\t')) q++;
        if (q[0] != '-' || q[1] != '-' || q[2] != '>') return -1;
        q += 3;
        while (q < le && (*q == ' ' || *q == '\t')) q++;
        if (parse_hms_ms(q, ',', &end_ms, &adv) != 0) return -1;
        p = nl;
        /* text lines until blank/EOF */
        const char *tstart = p;
        const char *tend = p;
        while (*p) {
            nl = next_line(p, &le);
            if (line_is_blank(p, le)) { p = nl; break; }
            tend = le;
            p = nl;
        }
        cue *c = track_push(t);
        if (!c) return -1; /* OOM: propagate as a parse failure, do not dereference NULL */
        c->index = idx;
        c->start_ms = start_ms;
        c->end_ms = end_ms;
        c->text = (tend > tstart) ? dup_range(tstart, tend) : dup_range(tstart, tstart);
    }
    return 0;
}

/* ------------------------------ WebVTT (.vtt) ------------------------------ */
/* Header "WEBVTT" on the first line; NOTE blocks skipped; cue blocks are an optional identifier line then a
 * "start --> end settings" line then text. Timestamps HH:MM:SS.mmm (dot). Cue settings after the timings
 * (position:/align:) are parsed and recorded on the last cue via out params below (kept minimal). */
static int parse_vtt(const char *buf, track *t) {
    const char *p = skip_bom(buf);
    track_init(t);
    const char *le;
    const char *nl = next_line(p, &le);
    /* first non-empty line must start with WEBVTT */
    if ((size_t)(le - p) < 6 || strncmp(p, "WEBVTT", 6) != 0) return -1;
    p = nl;
    while (*p) {
        nl = next_line(p, &le);
        if (line_is_blank(p, le)) { p = nl; continue; }
        /* NOTE comment block: skip until blank line */
        if ((size_t)(le - p) >= 4 && strncmp(p, "NOTE", 4) == 0) {
            p = nl;
            while (*p) { nl = next_line(p, &le); if (line_is_blank(p, le)) { p = nl; break; } p = nl; }
            continue;
        }
        /* Does this line contain "-->"? If not, it's a cue identifier; the next line is the timing. */
        const char *arrow = NULL;
        for (const char *s = p; s + 2 < le; s++) if (s[0]=='-'&&s[1]=='-'&&s[2]=='>') { arrow = s; break; }
        if (!arrow) {
            /* identifier line; consume it and read timing next */
            p = nl;
            nl = next_line(p, &le);
            arrow = NULL;
            for (const char *s = p; s + 2 < le; s++) if (s[0]=='-'&&s[1]=='-'&&s[2]=='>') { arrow = s; break; }
            if (!arrow) return -1;
        }
        long start_ms, end_ms; int adv;
        const char *q = p;
        while (q < le && (*q == ' ' || *q == '\t')) q++;
        if (parse_hms_ms(q, '.', &start_ms, &adv) != 0) return -1;
        q = arrow + 3;
        while (q < le && (*q == ' ' || *q == '\t')) q++;
        if (parse_hms_ms(q, '.', &end_ms, &adv) != 0) return -1;
        p = nl;
        /* text lines */
        const char *tstart = p, *tend = p;
        while (*p) { nl = next_line(p, &le); if (line_is_blank(p, le)) { p = nl; break; } tend = le; p = nl; }
        cue *c = track_push(t);
        if (!c) return -1; /* OOM: propagate as a parse failure, do not dereference NULL */
        c->index = -1;
        c->start_ms = start_ms;
        c->end_ms = end_ms;
        c->text = (tend > tstart) ? dup_range(tstart, tend) : dup_range(tstart, tstart);
    }
    return 0;
}

/* ------------------------------ ASS style table ------------------------------ */
typedef struct {
    char name[64];
    char fontname[64];
    int  fontsize;
    /* PrimaryColour &HAABBGGRR decomposed */
    unsigned aa, bb, gg, rr;
    int valid;
} ass_style;

/* ------------------------------ ASS parser ------------------------------ */
/* Parses [Script Info] / [V4+ Styles] / [Events]. Honors each section's "Format:" field ordering: the
 * Style line and the Dialogue line map columns by the declared format, not by fixed position. */
typedef struct {
    ass_style styles[16]; int nstyles;
    /* the Events Format column indices */
    int fmt_layer, fmt_start, fmt_end, fmt_style, fmt_text, fmt_count;
    int have_events_format;
} ass_doc;

/* split a comma line into up to `max` trimmed columns; returns count. For the LAST column when limit is
 * hit, the remainder (which may contain commas) is kept whole. cols[] point into a mutable copy `work`. */
static int split_cols(char *work, char **cols, int max, int last_is_rest) {
    int n = 0; char *s = work;
    while (n < max) {
        if (last_is_rest && n == max - 1) { cols[n++] = s; break; }
        char *comma = strchr(s, ',');
        if (!comma) { cols[n++] = s; break; }
        *comma = '\0';
        cols[n++] = s;
        s = comma + 1;
    }
    /* trim leading/trailing spaces on each column except leave rest column body intact for length checks */
    for (int i = 0; i < n; i++) {
        if (last_is_rest && i == n - 1) continue;
        char *c = cols[i];
        while (*c == ' ' || *c == '\t') c++;
        cols[i] = c;
        size_t l = strlen(c);
        while (l > 0 && (c[l-1] == ' ' || c[l-1] == '\t' || c[l-1] == '\r')) c[--l] = '\0';
    }
    return n;
}

static void ass_parse_style_format(const char *val, int *idx /*name,font,size,primary*/) {
    /* idx[0]=Name idx[1]=Fontname idx[2]=Fontsize idx[3]=PrimaryColour column positions */
    char work[512]; strncpy(work, val, sizeof work - 1); work[sizeof work - 1] = '\0';
    char *cols[64]; int n = split_cols(work, cols, 64, 0);
    idx[0] = idx[1] = idx[2] = idx[3] = -1;
    for (int i = 0; i < n; i++) {
        if (strcmp(cols[i], "Name") == 0) idx[0] = i;
        else if (strcmp(cols[i], "Fontname") == 0) idx[1] = i;
        else if (strcmp(cols[i], "Fontsize") == 0) idx[2] = i;
        else if (strcmp(cols[i], "PrimaryColour") == 0) idx[3] = i;
    }
}

/* parse &HAABBGGRR (ASS BGR order, alpha high byte). Returns 0/-1. */
static int parse_ass_colour(const char *s, unsigned *aa, unsigned *bb, unsigned *gg, unsigned *rr) {
    while (*s == ' ') s++;
    if (s[0] != '&' || (s[1] != 'H' && s[1] != 'h')) return -1;
    s += 2;
    unsigned long v = strtoul(s, NULL, 16);
    *rr = v & 0xFF;
    *gg = (v >> 8) & 0xFF;
    *bb = (v >> 16) & 0xFF;
    *aa = (v >> 24) & 0xFF;
    return 0;
}

static int parse_ass(const char *buf, track *t, ass_doc *doc) {
    const char *p = skip_bom(buf);
    track_init(t);
    memset(doc, 0, sizeof *doc);
    doc->fmt_layer = doc->fmt_start = doc->fmt_end = doc->fmt_style = doc->fmt_text = -1;

    enum { SEC_NONE, SEC_INFO, SEC_STYLES, SEC_EVENTS } sec = SEC_NONE;
    int sty_idx[4] = { -1, -1, -1, -1 };
    const char *le;
    while (*p) {
        const char *nl = next_line(p, &le);
        /* build a trimmed NUL-terminated copy of the line */
        size_t len = (size_t)(le - p);
        char line[1024];
        if (len >= sizeof line) len = sizeof line - 1;
        memcpy(line, p, len); line[len] = '\0';
        /* trim trailing CR/space */
        size_t l = strlen(line);
        while (l > 0 && (line[l-1] == '\r' || line[l-1] == ' ' || line[l-1] == '\t')) line[--l] = '\0';
        p = nl;
        if (line[0] == '\0') continue;
        if (line[0] == '[') {
            if (strncmp(line, "[Script Info]", 13) == 0) sec = SEC_INFO;
            else if (strstr(line, "Styles")) sec = SEC_STYLES;
            else if (strncmp(line, "[Events]", 8) == 0) sec = SEC_EVENTS;
            else sec = SEC_NONE;
            continue;
        }
        if (sec == SEC_STYLES) {
            if (strncmp(line, "Format:", 7) == 0) {
                ass_parse_style_format(line + 7, sty_idx);
            } else if (strncmp(line, "Style:", 6) == 0 && doc->nstyles < 16) {
                char work[512]; strncpy(work, line + 6, sizeof work - 1); work[sizeof work - 1] = '\0';
                char *cols[64]; int n = split_cols(work, cols, 64, 0);
                ass_style *s = &doc->styles[doc->nstyles];
                memset(s, 0, sizeof *s);
                if (sty_idx[0] >= 0 && sty_idx[0] < n) { strncpy(s->name, cols[sty_idx[0]], sizeof s->name - 1); }
                if (sty_idx[1] >= 0 && sty_idx[1] < n) { strncpy(s->fontname, cols[sty_idx[1]], sizeof s->fontname - 1); }
                if (sty_idx[2] >= 0 && sty_idx[2] < n) { s->fontsize = atoi(cols[sty_idx[2]]); }
                if (sty_idx[3] >= 0 && sty_idx[3] < n) {
                    parse_ass_colour(cols[sty_idx[3]], &s->aa, &s->bb, &s->gg, &s->rr);
                }
                s->valid = 1;
                doc->nstyles++;
            }
        } else if (sec == SEC_EVENTS) {
            if (strncmp(line, "Format:", 7) == 0) {
                char work[512]; strncpy(work, line + 7, sizeof work - 1); work[sizeof work - 1] = '\0';
                char *cols[64]; int n = split_cols(work, cols, 64, 0);
                doc->fmt_count = n;
                for (int i = 0; i < n; i++) {
                    if (strcmp(cols[i], "Layer") == 0) doc->fmt_layer = i;
                    else if (strcmp(cols[i], "Start") == 0) doc->fmt_start = i;
                    else if (strcmp(cols[i], "End") == 0) doc->fmt_end = i;
                    else if (strcmp(cols[i], "Style") == 0) doc->fmt_style = i;
                    else if (strcmp(cols[i], "Text") == 0) doc->fmt_text = i;
                }
                doc->have_events_format = 1;
            } else if (strncmp(line, "Dialogue:", 9) == 0 && doc->have_events_format) {
                char work[1024]; strncpy(work, line + 9, sizeof work - 1); work[sizeof work - 1] = '\0';
                char *cols[64];
                /* Text is the last format column and may contain commas -> keep the rest whole. */
                int n = split_cols(work, cols, doc->fmt_count, 1);
                if (doc->fmt_start >= n || doc->fmt_end >= n) continue;
                long start_ms, end_ms;
                if (parse_ass_time(cols[doc->fmt_start], &start_ms) != 0) return -1;
                if (parse_ass_time(cols[doc->fmt_end], &end_ms) != 0) return -1;
                cue *c = track_push(t);
                if (!c) return -1; /* OOM: propagate as a parse failure, do not dereference NULL */
                c->start_ms = start_ms; c->end_ms = end_ms; c->index = -1;
                c->layer = (doc->fmt_layer >= 0 && doc->fmt_layer < n) ? atoi(cols[doc->fmt_layer]) : 0;
                if (doc->fmt_style >= 0 && doc->fmt_style < n) c->style = dupstr(cols[doc->fmt_style]);
                if (doc->fmt_text >= 0 && doc->fmt_text < n) c->text = dupstr(cols[doc->fmt_text]);
                else c->text = dupstr("");
            }
        }
    }
    return 0;
}

/* Strip ASS override tags {\...} from a dialogue text, returning the plain-text LENGTH in bytes (not
 * content). Also collapses "\N"/"\n" hard-break escapes to one byte each. Writes nothing back. */
static size_t ass_plain_len(const char *text) {
    size_t len = 0;
    const char *s = text;
    while (*s) {
        if (*s == '{') {
            const char *e = strchr(s, '}');
            if (e) { s = e + 1; continue; }
        }
        if (s[0] == '\\' && (s[1] == 'N' || s[1] == 'n' || s[1] == 'h')) { len++; s += 2; continue; }
        len++; s++;
    }
    return len;
}

#endif /* SUBTITLE_PARSE_H */
