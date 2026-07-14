#!/usr/bin/env python3
# GlancesTuiCarpet.py -- industrial, pyte-verified assertion carpet for the glances curses TUI on
# StarryOS (4 arches). This is the hard/novel dimension: a real curses full-screen monitor driven
# through a live pseudo-terminal over a minutes-long soak, its raw ANSI output reconstructed by an
# offline terminal emulator (pyte) and asserted cell-by-cell.
#
# WHAT IS EXERCISED (NOT a "process starts" smoke):
#   * glances is launched inside a REAL pty (pty.fork -> initscr -> smkx alternate screen) with the
#     window size set via TIOCSWINSZ, so it takes its production terminal render path on StarryOS.
#   * A scripted key soak drives display + interaction + display-during-interaction:
#       - process sort hotkeys  m / c / a  (sort by memory / cpu / automatic) -- and we assert the
#         on-screen "Threads sorted ..." status line CHANGES accordingly (interaction really landed
#         and the screen repainted coherently).
#       - SS3 application-mode arrow keys (ESC O A/B/C/D) for process-cursor navigation. glances
#         does keypad(True) => DECCKM => it expects SS3 arrows; a CSI arrow would be mis-decoded to
#         a bare ESC and glances would QUIT. So "glances SURVIVED the whole arrow soak" is itself a
#         hard assertion that StarryOS's pty delivered SS3 correctly (verified fact).
#       - large-region redraw toggles (-2 left sidebar off/on, -3 quicklook off/on) to force full
#         dirty-block repaints, then assert the chrome comes back with no residue.
#   * The captured raw stream is fed to pyte and asserted three ways (see pyte_assert.py):
#       1. golden chrome present  (section labels + process-table header actually painted)
#       2. cross-frame stability  (every anchor token at the SAME row/col across all soak frames:
#                                  no flicker / no residue / no misaligned refresh)
#       3. differential-render invariant (stream fed in arbitrary split chunks == fed whole,
#                                  cell-for-cell -- no split/dropped escape sequence)
#
# Deterministic: fixed 200x50 terminal; environment-probe / network / container plugins that would
# block the first render on an offline SLIRP guest (ip public-IP lookup, cloud 169.254 metadata,
# containers/docker socket, ports/folders/connections scans) are disabled. Goldens are structural
# (labels/columns), never pinned to host-specific values.
#
# HARD-asserted sections (StarryOS renders every /proc source these read):
#   * CORE dashboard -- the process table (/proc/[pid]/stat + /proc/stat) and the top gauges MEM
#     (/proc/meminfo), LOAD (/proc/loadavg), TASKS (process count).
#   * left-sidebar procfs sections -- NETWORK (/proc/net/dev), DISK I/O (/proc/diskstats),
#     FILE SYS (/proc/mounts + statfs). glances paints a section ONLY when its plugin has non-empty
#     stats, so the section label appearing in the reconstructed screen is itself proof the psutil
#     plugin read live data: NETWORK carries the loopback interface, DISK I/O carries the root
#     virtio-blk disk "vda", and FILE SYS carries the ext4 root mount. These are asserted as HARD
#     gates (not best-effort) -- the same three sections the headless carpet proves numerically.
#     The pyte-soak stability anchors use the always-present CORE process-table header (the sidebar
#     rows churn as stats update, so they are not used as positional anchors).
#
# Emits `GTUI_RESULT ok=<N> fail=<F>` and, only when F==0, `GTUI_DONE`.
import os, sys

HERE = os.path.dirname(os.path.abspath(__file__))
# programs/ (pty_tui_drive.py, pyte_assert.py) is a sibling of python/ in the app tree; on-target
# both are staged under the same run dir. Support both layouts.
for cand in (os.path.join(HERE, "..", "programs"), HERE, "/root/monitor/programs", "/root/monitor"):
    if os.path.isfile(os.path.join(cand, "pty_tui_drive.py")):
        sys.path.insert(0, cand)
        break
import pty_tui_drive as ptd
import pyte_assert as pa

GLANCES = os.environ.get("GLANCES_BIN", "glances")
COLS = int(os.environ.get("MONITOR_TUI_COLS", "200"))
ROWS = int(os.environ.get("MONITOR_TUI_ROWS", "50"))
# min dwell per step (keeps a real soak even on fast arches); big per-step max-wait budget so slow
# TCG arches (aa/rv/loong) can finish rendering before we capture (content-driven, not time-driven).
SETTLE = float(os.environ.get("MONITOR_TUI_SETTLE", "4"))          # min dwell for the first-render settle
DWELL = float(os.environ.get("MONITOR_TUI_DWELL", "2"))            # min dwell per interaction
SETTLE_WAIT = float(os.environ.get("MONITOR_TUI_SETTLE_WAIT", "90"))  # max wait for the FIRST full render
STEP_WAIT = float(os.environ.get("MONITOR_TUI_STEP_WAIT", "45"))      # max wait per interaction render

# Plugins that do network / socket / external probing and would block the FIRST render on an
# offline guest (proven on host: sensors blocked; on a SLIRP guest ip/cloud/containers block).
# Disabling them is legitimate -- we test the TUI render+interaction, not those probes.
DISABLE = "ip,cloud,containers,docker,folders,ports,smart,wifi,gpu,connections,sensors"

CMD = [GLANCES, "--time", "1", "--disable-check-update", "--disable-plugin", DISABLE]

# CONTENT-DRIVEN capture: each step waits UNTIL the expected content is rendered (or a generous
# per-arch timeout) before capturing, so the same script is timing-robust on x86 AND slow TCG arches.
# The process-table header is the readiness signal for the layout; the sort steps wait for the sort
# status line to actually show the new sort keyword. Phase A = constant layout (stability + sort
# interaction + SS3 arrows). Phase B = large-region redraw toggles.
HDR = ["CPU%", "MEM%", "PID"]           # process-table header = "the dashboard has rendered" signal
def _w(tokens, mx, mn):
    return {"wait_for": tokens, "max_wait": mx, "min_dwell": mn}
SCRIPT = [
    ("A_settle",   b"",           _w(HDR,            SETTLE_WAIT, SETTLE)),
    ("A_sort_mem", b"m",          _w(["by memory"],  STEP_WAIT,   DWELL)),
    ("A_sort_cpu", b"c",          _w(["by CPU"],     STEP_WAIT,   DWELL)),
    ("A_sort_auto",b"a",          _w(["automatically"], STEP_WAIT, DWELL)),
    ("A_obs",      b"",           _w(HDR,            STEP_WAIT,   DWELL)),
    ("A_down",     ptd.SS3_DOWN,  _w(HDR,            STEP_WAIT,   DWELL)),
    ("A_down2",    ptd.SS3_DOWN,  _w(HDR,            STEP_WAIT,   DWELL)),
    ("A_up",       ptd.SS3_UP,    _w(HDR,            STEP_WAIT,   DWELL)),
    ("A_right",    ptd.SS3_RIGHT, _w(HDR,            STEP_WAIT,   DWELL)),
    ("A_left",     ptd.SS3_LEFT,  _w(HDR,            STEP_WAIT,   DWELL)),
    ("B_sidebar_off", b"2",       _w(HDR,            STEP_WAIT,   DWELL)),
    ("B_sidebar_on",  b"2",       _w(HDR,            STEP_WAIT,   DWELL)),
    ("B_quicklook_off", b"3",     _w(HDR,            STEP_WAIT,   DWELL)),
    ("B_quicklook_on",  b"3",     _w(HDR,            STEP_WAIT,   DWELL)),
    ("B_final",    b"",           _w(HDR,            STEP_WAIT,   DWELL)),
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


def sorted_line(disp):
    for line in disp:
        j = line.find("sorted")
        if j >= 0:
            return line[max(0, j - 8):j + 45].strip()
    return ""


def main():
    print("=== GlancesTuiCarpet: pyte-verified curses TUI soak (%dx%d, content-driven capture) ==="
          % (COLS, ROWS))
    res = ptd.drive(CMD, SCRIPT, cols=COLS, rows=ROWS, quit_key=b"q")
    raw = res["raw"]
    # per-step wait outcome: which steps rendered their expected content in time (met=True) vs timed
    # out (met=False -> a real failure will surface below since the captured frame lacks the content).
    timed_out = [lbl for lbl, _off, met in res["checkpoints"] if met is False]
    print("  driver: survived=%s dead=%s rawbytes=%d sent=%d checkpoints=%d timed_out=%s"
          % (res["survived"], res["dead"], len(raw), len(res["sent"]), len(res["checkpoints"]), timed_out))

    # 1. SS3 survival -- glances never quit across the arrow + hotkey soak (the DECCKM/SS3 fact).
    check(res["survived"], "glances SURVIVED the full SS3-arrow + hotkey soak (no bare-ESC quit)")
    check(res["dead"] != "exit:1" and (res["dead"] is None or not res["dead"].startswith("exit:") or res["dead"] == "exit:0"),
          "glances did not crash mid-soak (dead=%s)" % res["dead"])

    # 2. it actually painted a full-screen dashboard (not blank / not a traceback).
    check(len(raw) > 4000, "TUI painted a substantial ANSI stream (>4000 bytes, got %d)" % len(raw))

    # 2b. the FIRST full render happened within the (generous, per-arch) settle budget -- if the
    #     process table never rendered in time, that is a REAL failure (据实), not a silent pass.
    settle_met = next((met for lbl, _o, met in res["checkpoints"] if lbl == "A_settle"), None)
    check(settle_met is not False, "process table rendered within the settle budget (%.0fs); timed_out=%s"
          % (SETTLE_WAIT, timed_out))

    # reconstruct per-checkpoint frames. Content assertions use the LAST PRE-QUIT checkpoint frame
    # (B_final): after 'q', curses endwin() exits the alternate screen and the primary buffer is
    # blank, so full_screen(raw) is not the live dashboard. The differential-render invariant below
    # deliberately still runs over the WHOLE raw (byte-stream integrity, teardown included).
    frames = []
    for label, off, _met in res["checkpoints"]:
        frames.append((label, list(pa.full_screen(raw[:off], COLS, ROWS).display)))
    final_disp = frames[-1][1] if frames else list(pa.full_screen(raw, COLS, ROWS).display)

    # 3. golden chrome present in the final painted frame (structural labels, never host values).
    #    HARD: the CORE dashboard that renders from the /proc StarryOS reliably provides -- the process
    #    table (/proc/[pid]/stat + /proc/stat) and the top gauges MEM (/proc/meminfo), LOAD
    #    (/proc/loadavg), TASKS (process count). These are the same core sources the headless carpet
    #    already proves on-target.
    header_tokens = ["CPU%", "MEM%", "PID", "USER", "TIME+", "Command"]
    hf, hmiss = pa.tokens_present(final_disp, header_tokens)
    check(not hmiss, "process-table header tokens rendered %s (missing=%s)" % (header_tokens, hmiss))
    core_sections = ["TASKS", "LOAD", "MEM"]
    cf, cmiss = pa.tokens_present(final_disp, core_sections)
    check(not cmiss, "core dashboard sections rendered %s (missing=%s)" % (core_sections, cmiss))

    # 3b. HARD: the left-sidebar procfs sections. glances paints a section ONLY when its plugin has
    #     non-empty stats (network/diskio/fs early-return with no stats), so a rendered section label
    #     is proof the psutil plugin read live data from StarryOS /proc:
    #       NETWORK  <- psutil.net_io_counters  (/proc/net/dev)
    #       DISK I/O <- psutil.disk_io_counters (/proc/diskstats)
    #       FILE SYS <- psutil.disk_partitions  (/proc/mounts) + statfs
    for opt in ["NETWORK", "DISK I/O", "FILE SYS"]:
        print("  section %-9s rendered=%s" % (opt, pa.find_token(final_disp, opt)[0] >= 0))
    sidebar_sections = ["NETWORK", "DISK I/O", "FILE SYS"]
    sf, smiss = pa.tokens_present(final_disp, sidebar_sections)
    check(not smiss, "left-sidebar procfs sections rendered %s (missing=%s)" % (sidebar_sections, smiss))
    # concrete per-source data tokens inside those sections (glances only prints them with real stats):
    #   'vda'  -> the root virtio-blk disk name, shown by BOTH DISK I/O and the FILE SYS root row
    #             "/ (vda)"; its presence proves /proc/diskstats + /proc/mounts data reached the render.
    #   'eth0' -> the ethernet interface row in NETWORK, proving /proc/net/dev data reached the render.
    data_tokens = ["vda", "eth0"]
    df, dmiss = pa.tokens_present(final_disp, data_tokens)
    check(not dmiss, "sidebar sections carry live procfs data %s (missing=%s)" % (data_tokens, dmiss))

    # 4. display-during-interaction: the sort hotkeys changed the on-screen sort status line.
    ck = {lbl: dsp for lbl, dsp in frames}
    if "A_sort_mem" in ck:
        line = sorted_line(ck["A_sort_mem"])
        check("memory" in line.lower(), "after 'm' the sort line reads memory: %r" % line)
    else:
        check(False, "A_sort_mem checkpoint captured")
    if "A_sort_cpu" in ck:
        line = sorted_line(ck["A_sort_cpu"])
        check("cpu" in line.lower(), "after 'c' the sort line reads CPU: %r" % line)
    else:
        check(False, "A_sort_cpu checkpoint captured")
    if "A_sort_auto" in ck:
        line = sorted_line(ck["A_sort_auto"])
        check("automatically" in line.lower(), "after 'a' the sort line reads automatically: %r" % line)
    else:
        check(False, "A_sort_auto checkpoint captured")

    # 5. cross-frame stability over the CONSTANT-layout Phase A frames: chrome anchors never flicker,
    #    move, or leave residue (no misaligned refresh) as the dynamic stats churn + cursor moves.
    #    Anchors are the CORE process-table header tokens -- always rendered (the process list is
    #    core) and positionally stable within a run -- so the invariant holds on a minimal guest too
    #    (network/diskio anchors would be unstable there and are deliberately NOT used).
    phaseA = [(lbl, dsp) for lbl, dsp in frames if lbl.startswith("A_")]
    anchors = ["CPU%", "MEM%", "PID", "USER", "Command"]
    viol = pa.stability_violations(phaseA, anchors)
    check(not viol, "Phase-A chrome stable across %d frames (no flicker/residue/misalign): %s"
          % (len(phaseA), viol[:3]))

    # 5b. after the Phase-B large-region toggles-and-back (-2 sidebar, -3 quicklook), the CORE chrome
    #     REDREW with no residue (assert the process-table header + core sections came back).
    bf, bmiss = pa.tokens_present(final_disp, ["PID", "Command", "MEM", "LOAD"])
    check(not bmiss, "core chrome redrew after -2/-3 toggles (no residue); missing=%s" % bmiss)

    # 6. differential-render invariant: feeding the stream split at every checkpoint boundary yields
    #    a cell-for-cell IDENTICAL screen to feeding it whole (no split/dropped escape sequence).
    splits = [off for _, off, _ in res["checkpoints"]]
    inc = pa.incremental_screen(raw, splits, COLS, ROWS)
    whole = pa.full_screen(raw, COLS, ROWS)
    eq, diffrow, detail = pa.screens_equal(inc, whole)
    check(eq, "differential-render invariant: chunked==whole (first diff %s: %s)" % (diffrow, detail))

    print("GTUI_RESULT ok=%d fail=%d" % (_ok, _fail))
    if _fail == 0:
        print("GTUI_DONE")
        return 0
    # on failure, dump the final frame for diagnosis
    print("--- final frame (nonempty rows) ---")
    for i, l in enumerate(final_disp):
        t = l.rstrip()
        if t:
            print("%2d|%s" % (i, t[:150]))
    return 1


if __name__ == "__main__":
    sys.exit(main())
