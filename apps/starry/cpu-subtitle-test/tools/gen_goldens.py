#!/usr/bin/env python3
# gen_goldens.py - host-side cross-check for the cpu-subtitle-test carpet's STRUCTURAL goldens.
#
# The C cells carry the pinned structural constants for the real .srt/.ass files (cue/dialogue count,
# first/last timestamp in ms, monotonic ordering, valid UTF-8). This script recomputes those STRUCTURAL
# properties from the shipped files so a maintainer can verify the goldens baked into subtitle_realassets.c
# without trusting the C parser alone. It prints STRUCTURE ONLY - it never emits the dialogue text (the
# files are copyrighted; the carpet asserts structure, not content).
#
# Usage: gen_goldens.py <render-assets/subtitles dir>
import sys, re, os

def srt_golden(path):
    data = open(path, 'rb').read()
    data.decode('utf-8')  # raises if not valid UTF-8
    text = data.decode('utf-8')
    blocks = re.split(r'\n\s*\n', text.strip())
    cues = []
    for b in blocks:
        lines = b.splitlines()
        if not lines:
            continue
        idx = int(lines[0].strip())
        m = re.match(r'(\d\d):(\d\d):(\d\d),(\d\d\d)\s*-->\s*(\d\d):(\d\d):(\d\d),(\d\d\d)', lines[1])
        sh, sm, ss, sms, eh, em, es, ems = map(int, m.groups())
        start = ((sh*60+sm)*60+ss)*1000 + sms
        end = ((eh*60+em)*60+es)*1000 + ems
        cues.append((idx, start, end))
    idxs = [c[0] for c in cues]
    return {
        'cues': len(cues),
        'first_index': idxs[0], 'last_index': idxs[-1],
        'first_start_ms': cues[0][1], 'last_end_ms': cues[-1][2],
        'index_contiguous': idxs == list(range(idxs[0], idxs[0]+len(idxs))),
        'starts_monotonic': all(cues[i][1] <= cues[i+1][1] for i in range(len(cues)-1)),
        'end_ge_start': all(c[2] >= c[1] for c in cues),
        'max_end_ms': max(c[2] for c in cues),
    }

def ass_ms(t):
    m = re.match(r'(\d+):(\d\d):(\d\d)\.(\d\d)', t)
    h, mm, ss, cc = map(int, m.groups())
    return ((h*60+mm)*60+ss)*1000 + cc*10

def ass_golden(path):
    data = open(path, 'rb').read()
    data.decode('utf-8')
    text = data.decode('utf-8')
    ev_fmt = sty_fmt = None
    dialogues, styles = [], []
    for ln in text.splitlines():
        if ln.startswith('Format:') and 'Fontname' in ln and sty_fmt is None:
            sty_fmt = [x.strip() for x in ln[7:].split(',')]
        if ln.startswith('Format:') and 'Text' in ln:
            ev_fmt = [x.strip() for x in ln[7:].split(',')]
        if ln.startswith('Style:'):
            styles.append([x.strip() for x in ln[6:].split(',')])
        if ln.startswith('Dialogue:'):
            parts = ln[9:].split(',', len(ev_fmt)-1)
            dialogues.append([p.strip() if i < len(ev_fmt)-1 else p for i, p in enumerate(parts)])
    li, si, ei, styi = (ev_fmt.index(k) for k in ('Layer', 'Start', 'End', 'Style'))
    starts = [ass_ms(d[si]) for d in dialogues]
    ends = [ass_ms(d[ei]) for d in dialogues]
    sty = styles[0]
    return {
        'dialogues': len(dialogues),
        'first_start_ms': starts[0], 'last_end_ms': ends[-1],
        'starts_monotonic': all(starts[i] <= starts[i+1] for i in range(len(starts)-1)),
        'end_ge_start': all(ends[i] >= starts[i] for i in range(len(starts))),
        'max_end_ms': max(ends),
        'all_layer_0': all(d[li] == '0' for d in dialogues),
        'all_style_default': all(d[styi] == 'Default' for d in dialogues),
        'style_count': len(styles),
        'style_name': sty[sty_fmt.index('Name')],
        'style_fontname': sty[sty_fmt.index('Fontname')],
        'style_fontsize': sty[sty_fmt.index('Fontsize')],
        'style_primary': sty[sty_fmt.index('PrimaryColour')],
    }

def main():
    d = sys.argv[1] if len(sys.argv) > 1 else '.'
    srt = os.path.join(d, 'tashouheng.srt')
    ass = os.path.join(d, 'badapple.ass')
    if os.path.isfile(srt):
        print("SRT tashouheng.srt:", srt_golden(srt))
    else:
        print("SRT tashouheng.srt: absent (real-asset leg honest-skips)")
    if os.path.isfile(ass):
        print("ASS badapple.ass:", ass_golden(ass))
    else:
        print("ASS badapple.ass: absent (real-asset leg honest-skips)")

if __name__ == '__main__':
    main()
