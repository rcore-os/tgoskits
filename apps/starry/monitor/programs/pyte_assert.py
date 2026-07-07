#!/usr/bin/env python3
# pyte_assert.py -- offline terminal-emulation assertions over a captured raw ANSI stream.
#
# Feeds a captured raw byte stream to pyte (a pure-python VT/ANSI terminal emulator) to reconstruct
# the exact on-screen cell matrix the TUI painted, then asserts against it. Three families of check:
#
#   1. golden-frame content  -- required "static chrome" tokens (section labels, table headers) are
#      present in the reconstructed screen (proves the TUI actually rendered its dashboard, not a
#      blank/garbled screen).
#   2. cross-frame stability  -- each anchor token lands at the SAME (row, col) at every checkpoint
#      across the whole soak: no flicker (chrome never disappears), no residue (no stale duplicate),
#      no misaligned refresh (columns never shift). This is the differential-across-time invariant.
#   3. differential-render invariant -- feeding the byte stream in arbitrary split chunks yields a
#      cell-for-cell IDENTICAL screen to feeding it whole. A mismatch means an escape sequence was
#      split/dropped/corrupted (capture or tty delivery bug). This is the differential-render
#      (incremental == full-redraw) invariant.
import pyte


def full_screen(raw, cols, rows):
    s = pyte.Screen(cols, rows)
    st = pyte.ByteStream(s)
    st.feed(raw)
    return s


def incremental_screen(raw, split_offsets, cols, rows):
    """Feed `raw` into ONE persistent Screen, split at each offset in split_offsets (arbitrary
    boundaries that mimic os.read() chunking / packet arrival)."""
    s = pyte.Screen(cols, rows)
    st = pyte.ByteStream(s)
    prev = 0
    for off in sorted(set(o for o in split_offsets if 0 < o < len(raw))):
        st.feed(raw[prev:off])
        prev = off
    st.feed(raw[prev:])
    return s


def display_lines(screen):
    return list(screen.display)


def find_token(disp, token):
    for r, line in enumerate(disp):
        c = line.find(token)
        if c >= 0:
            return (r, c)
    return (-1, -1)


def tokens_present(disp, tokens):
    """Return {token: (row,col)} for tokens found, and a list of missing tokens."""
    found = {}
    missing = []
    for t in tokens:
        rc = find_token(disp, t)
        if rc[0] >= 0:
            found[t] = rc
        else:
            missing.append(t)
    return found, missing


def screens_equal(a, b):
    """Cell-for-cell compare via the rendered display lines. Returns (bool, first_diff_row, detail)."""
    da, db = list(a.display), list(b.display)
    if len(da) != len(db):
        return (False, -1, "row count %d != %d" % (len(da), len(db)))
    for r in range(len(da)):
        if da[r] != db[r]:
            return (False, r, "row %d: %r != %r" % (r, da[r][:60], db[r][:60]))
    return (True, -1, "")


def stability_violations(frames, anchors):
    """frames = list of (label, disp). anchors = list of tokens whose (row,col) must be identical
    across every frame in which the TUI is fully painted. Returns list of violation strings.

    An anchor is required to be present in EVERY frame (absence = flicker/erased chrome) and at the
    SAME coordinate (movement = misaligned refresh / residue)."""
    violations = []
    for tok in anchors:
        coords = []
        for label, disp in frames:
            rc = find_token(disp, tok)
            coords.append((label, rc))
        present = [rc for _, rc in coords if rc[0] >= 0]
        if len(present) != len(coords):
            missing_at = [lbl for lbl, rc in coords if rc[0] < 0]
            violations.append("anchor %r missing at frames %s (flicker/erased chrome)" % (tok, missing_at))
            continue
        first = present[0]
        for label, rc in coords:
            if rc != first:
                violations.append("anchor %r moved: %s@%s != baseline %s (misaligned refresh)"
                                  % (tok, label, rc, first))
                break
    return violations
