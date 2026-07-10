#!/usr/bin/env python3
# HtopCarpet.py -- industrial, pyte-verified assertion carpet for the htop process-monitor TUI on
# StarryOS (4 arches). Like the glances TUI carpet, htop is a full-screen ncurses program driven
# through a live pseudo-terminal over a soak, its raw ANSI output reconstructed by an offline
# terminal emulator (pyte) and asserted cell-by-cell -- not a "process starts" smoke.
#
# WHAT IS EXERCISED:
#   * htop is launched inside a REAL pty (pty.fork -> initscr -> keypad(True) -> alternate screen)
#     with the window size set via TIOCSWINSZ, so it takes its production render path on StarryOS.
#   * The dashboard reads the /proc sources StarryOS renders: the process table from
#     /proc/[pid]/{stat,statm,cmdline,comm}, the CPU meter from /proc/stat, the Mem/Swp meters from
#     /proc/meminfo, the Load-average meter from /proc/loadavg and the Uptime meter from /proc/uptime.
#   * A scripted key soak drives every documented function surface and asserts the resulting screen:
#       - sort hotkeys  M / P / T  (by MEM% / CPU% / TIME+) and the F6 sort-field panel;
#       - tree view toggle  t  (hierarchical process view) back to the flat list;
#       - F2 Setup (Meters / Display options / Colors / Columns configuration screen);
#       - F6 SortBy field panel;  F9 Kill signal panel (SIGTERM/SIGKILL, cancelled -- no signal sent);
#       - F3 Search and F4 Filter incremental input bars;  F1 Help screen (version banner + legend);
#       - F10 to quit.
#     The F-keys are the xterm-256color terminfo sequences; htop only opens a panel when the exact
#     bytes arrive, so a panel appearing after an F-key is itself proof StarryOS's pty delivered it
#     intact (the function-key analogue of the SS3-arrow fact proven by the glances TUI carpet).
#   * The captured raw stream is fed to pyte and asserted three ways (see pyte_assert.py):
#       1. golden chrome present   (process-table header + meter labels + function bar painted)
#       2. cross-frame stability   (header anchors at the SAME row/col across all flat-list frames)
#       3. differential-render invariant (stream fed in arbitrary split chunks == fed whole)
#
# Deterministic: fixed 200x50 terminal; a fresh empty HOME so htop uses its built-in default layout
# (no stale htoprc). Goldens are structural (labels/columns/panel names), never host-specific values.
#
# Emits `HTOP_RESULT ok=<N> fail=<F>` and, only when F==0, `HTOP_DONE`.
import os, sys, tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
for cand in (os.path.join(HERE, "..", "programs"), HERE, "/root/monitor/programs", "/root/monitor"):
    if os.path.isfile(os.path.join(cand, "pty_tui_drive.py")):
        sys.path.insert(0, cand)
        break
import pty_tui_drive as ptd
import pyte_assert as pa

HTOP = os.environ.get("HTOP_BIN", "htop")
COLS = int(os.environ.get("MONITOR_TUI_COLS", "200"))
ROWS = int(os.environ.get("MONITOR_TUI_ROWS", "50"))
SETTLE = float(os.environ.get("MONITOR_HTOP_SETTLE", "4"))
DWELL = float(os.environ.get("MONITOR_HTOP_DWELL", "2"))
SETTLE_WAIT = float(os.environ.get("MONITOR_HTOP_SETTLE_WAIT", "90"))
STEP_WAIT = float(os.environ.get("MONITOR_HTOP_STEP_WAIT", "45"))
DEBUG = os.environ.get("MONITOR_HTOP_DEBUG", "") not in ("", "0")

# -d N : update delay in tenths of a second (1s); -C : mono, no color escapes to muddy the capture.
CMD = [HTOP, "-d", "10", "-C"]

# process-table header = "the dashboard has rendered" readiness signal.
HDR = ["PID", "CPU%", "Command"]


def _w(tokens, mx, mn):
    return {"wait_for": tokens, "max_wait": mx, "min_dwell": mn}


# Phase FLAT = constant flat-list layout (settle + sort hotkeys) -- used for the stability invariant.
# Then the tree toggle, then each panel (opened by its F-key, asserted, cancelled back to the list).
SCRIPT = [
    ("settle",     b"",                _w(HDR,                    SETTLE_WAIT, SETTLE)),
    ("sort_mem",   b"M",               _w(HDR,                    STEP_WAIT,   DWELL)),
    ("sort_cpu",   b"P",               _w(HDR,                    STEP_WAIT,   DWELL)),
    ("sort_time",  b"T",               _w(HDR,                    STEP_WAIT,   DWELL)),
    ("flat_obs",   b"",                _w(HDR,                    STEP_WAIT,   DWELL)),
    ("tree_on",    b"t",               _w(["PID", "Command"],     STEP_WAIT,   DWELL)),
    ("tree_off",   b"t",               _w(HDR,                    STEP_WAIT,   DWELL)),
    ("setup",      ptd.F2,             _w(["Meters"],             STEP_WAIT,   DWELL)),
    ("setup_close",ptd.F10,            _w(HDR,                    STEP_WAIT,   DWELL)),
    ("sortby",     ptd.F6,             _w(["Sort by"],            STEP_WAIT,   DWELL)),
    ("sortby_close",ptd.ESC,           _w(HDR,                    STEP_WAIT,   DWELL)),
    ("kill",       ptd.F9,             _w(["SIGTERM"],            STEP_WAIT,   DWELL)),
    ("kill_close", ptd.ESC,            _w(HDR,                    STEP_WAIT,   DWELL)),
    ("search",     ptd.F3 + b"htop",   _w(["Search"],             STEP_WAIT,   DWELL)),
    ("search_close",ptd.ESC,           _w(HDR,                    STEP_WAIT,   DWELL)),
    ("filter",     ptd.F4 + b"htop",   _w(["Filter"],             STEP_WAIT,   DWELL)),
    ("filter_close",ptd.ESC,           _w(HDR,                    STEP_WAIT,   DWELL)),
    ("help",       ptd.F1,             _w(["htop"],               STEP_WAIT,   DWELL)),
    ("help_close", b" ",               _w(HDR,                    STEP_WAIT,   DWELL)),
    ("final",      b"",                _w(HDR,                    STEP_WAIT,   DWELL)),
]

_ok = 0
_fail = 0


def check(cond, label):
    global _ok, _fail
    if cond:
        _ok += 1
    else:
        _fail += 1
        print("  FAIL %s" % label)


def dump(label, disp):
    print("  --- frame %s ---" % label)
    for i, l in enumerate(disp):
        t = l.rstrip()
        if t:
            print("  %2d|%s" % (i, t[:150]))


def main():
    print("=== HtopCarpet: pyte-verified htop TUI soak (%dx%d, content-driven capture) ===" % (COLS, ROWS))
    home = tempfile.mkdtemp(prefix="htop-home-")
    res = ptd.drive(CMD, SCRIPT, cols=COLS, rows=ROWS, quit_key=ptd.F10,
                    env_extra={"HOME": home, "XDG_CONFIG_HOME": os.path.join(home, ".config")})
    raw = res["raw"]
    timed_out = [lbl for lbl, _off, met in res["checkpoints"] if met is False]
    print("  driver: survived=%s dead=%s rawbytes=%d sent=%d checkpoints=%d timed_out=%s"
          % (res["survived"], res["dead"], len(raw), len(res["sent"]), len(res["checkpoints"]), timed_out))

    # 1. survival across the whole F-key + hotkey soak (no crash, no bare-ESC mis-decode quit).
    check(res["survived"], "htop SURVIVED the full F-key + hotkey soak")
    check(res["dead"] is None or not res["dead"].startswith("exit:") or res["dead"] == "exit:0",
          "htop did not crash mid-soak (dead=%s)" % res["dead"])
    check(len(raw) > 4000, "htop painted a substantial ANSI stream (>4000 bytes, got %d)" % len(raw))

    settle_met = next((met for lbl, _o, met in res["checkpoints"] if lbl == "settle"), None)
    check(settle_met is not False, "dashboard rendered within the settle budget (%.0fs); timed_out=%s"
          % (SETTLE_WAIT, timed_out))

    frames = {}
    order = []
    for label, off, _met in res["checkpoints"]:
        disp = list(pa.full_screen(raw[:off], COLS, ROWS).display)
        frames[label] = disp
        order.append((label, disp))
    final = frames.get("final") or (order[-1][1] if order else [])

    if DEBUG:
        for label in ("settle", "tree_on", "setup", "sortby", "kill", "search", "filter", "help", "final"):
            if label in frames:
                dump(label, frames[label])

    # 2. golden chrome on the final flat-list frame: the process-table header (structural columns).
    header_tokens = ["PID", "USER", "CPU%", "MEM%", "TIME+", "Command"]
    hf, hmiss = pa.tokens_present(final, header_tokens)
    check(not hmiss, "process-table header rendered %s (missing=%s)" % (header_tokens, hmiss))

    # 3. the top meters -- each proves a distinct /proc source is read:
    #      Mem  <- /proc/meminfo   Swp <- /proc/meminfo   Tasks <- /proc/stat + process table
    #      Load average <- /proc/loadavg   Uptime <- /proc/uptime
    meter_tokens = ["Mem", "Swp", "Tasks", "Load average", "Uptime"]
    mf, mmiss = pa.tokens_present(final, meter_tokens)
    check(not mmiss, "top meters rendered %s (missing=%s)" % (meter_tokens, mmiss))

    # 4. the function-key bar (the documented action surface).
    fbar = ["Help", "Setup", "Search", "Filter", "Tree", "Kill", "Quit"]
    ff, fmiss = pa.tokens_present(final, fbar)
    check(not fmiss, "function-key bar rendered %s (missing=%s)" % (fbar, fmiss))

    # 5. F2 Setup screen: the configuration categories (proves F2 delivered + the panel painted).
    setup = frames.get("setup", [])
    su_tokens = ["Meters", "Display options", "Colors"]
    suf, sumiss = pa.tokens_present(setup, su_tokens)
    check(not sumiss, "F2 setup screen shows config categories %s (missing=%s)" % (su_tokens, sumiss))

    # 6. F6 SortBy field panel: its 'Sort by' title appears while the process list stays visible.
    sortby = frames.get("sortby", [])
    check(pa.find_token(sortby, "Sort by")[0] >= 0 and pa.find_token(sortby, "PID")[0] >= 0,
          "F6 sort-field panel opened ('Sort by' title + field list)")

    # 7. F9 Kill signal panel: the 'Send signal:' title + named signals (cancelled with Esc, so NO
    #    signal is ever delivered to any process).
    kill = frames.get("kill", [])
    kt = ["Send signal:", "SIGTERM", "SIGKILL"]
    kf, kmiss = pa.tokens_present(kill, kt)
    check(not kmiss, "F9 kill panel lists signals %s (missing=%s)" % (kt, kmiss))

    # 8. F3 Search + F4 Filter incremental bars (the ': ' prompt only appears in input mode).
    check(pa.find_token(frames.get("search", []), "Search:")[0] >= 0,
          "F3 search bar shows the 'Search:' prompt")
    check(pa.find_token(frames.get("filter", []), "Filter:")[0] >= 0,
          "F4 filter bar shows the 'Filter:' prompt")

    # 9. F1 Help screen: the version banner (only htop's help/version prints the 'htop <ver>' line).
    helpf = frames.get("help", [])
    import re as _re
    check(any(_re.search(r"htop\s+\d+\.\d+", l) for l in helpf), "F1 help screen shows the htop version banner")

    # 10. cross-frame stability over the FLAT-list frames: header anchors never flicker / move / leave
    #     residue as rows churn and the sort column changes (sorting reorders rows, not column labels).
    flat_labels = {"settle", "sort_mem", "sort_cpu", "sort_time", "flat_obs", "tree_off", "final"}
    flat = [(lbl, dsp) for lbl, dsp in order if lbl in flat_labels]
    anchors = ["PID", "CPU%", "MEM%", "Command"]
    viol = pa.stability_violations(flat, anchors)
    check(not viol, "flat-list chrome stable across %d frames (no flicker/residue/misalign): %s"
          % (len(flat), viol[:3]))

    # 11. differential-render invariant: chunked feed == whole feed, cell-for-cell.
    splits = [off for _, off, _ in res["checkpoints"]]
    inc = pa.incremental_screen(raw, splits, COLS, ROWS)
    whole = pa.full_screen(raw, COLS, ROWS)
    eq, diffrow, detail = pa.screens_equal(inc, whole)
    check(eq, "differential-render invariant: chunked==whole (first diff %s: %s)" % (diffrow, detail))

    print("HTOP_RESULT ok=%d fail=%d" % (_ok, _fail))
    if _fail == 0:
        print("HTOP_DONE")
        return 0
    dump("final", final)
    return 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as e:
        import traceback
        traceback.print_exc()
        print("HTOP_RESULT ok=%d fail=%d" % (_ok, _fail + 1))
        sys.exit(1)
