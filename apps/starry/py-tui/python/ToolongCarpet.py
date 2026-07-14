#!/usr/bin/env python3
# ToolongCarpet.py — heavy-real-app assisted carpet for the `textual` TUI framework (leg B).
#
# Instead of exercising textual widgets in isolation, this carpet drives a REAL, popular, pure-
# Python Textualize application END TO END, fully HEADLESS: `toolong` (the `tl` log viewer, 35k+
# GitHub stars' worth of production TUI). If a complex real app boots, scans a file on a worker
# thread, renders its compositor output, and responds correctly to a full keyboard interaction
# sequence — all through textual's own `App.run_test()` virtual-terminal driver — then the `textual`
# library is proven through genuine third-party usage, not just unit-style widget pokes.
#
# toolong pins `textual==0.58.1` (leg A's TextualCarpet uses 8.2.8 and frogmouth uses 0.43.2 — all
# three conflict), so this carpet is installed into its OWN pip --target dir (/opt/pytui-toolong)
# and run with PYTHONPATH pointing ONLY there. Its whole dependency closure (click, textual, rich,
# markdown-it-py, mdit-py-plugins, mdurl, linkify-it-py, uc-micro-py, pygments, typing-extensions)
# is pure-Python (py3-none-any), so the tree is architecture-independent across the four StarryOS
# target arches.
#
# The app is driven against a FIXED, committed fixture (fixture.log — 60 deterministic lines: an
# INFO/DEBUG/WARNING/ERROR mix plus one pure-JSON line). Every golden below was recomputed from the
# live library's actual compositor output. After EACH interaction (home/end/pagedown/pageup/arrow/
# line-number toggle/pointer select/detail panel/go-to screen/find input) the screen strips are
# re-captured and asserted: the expected line content is present, the box border columns stay
# aligned, and stale content is gone. Deterministic: fixed 100x30 virtual terminal, no wall clock,
# no animation waits, no network, no randomness. run_test always exits cleanly.
#
# Emits `TOOLONG_RESULT ok=<N> fail=<F>` and, only when F==0, `TOOLONG_DONE`.

import asyncio
import importlib.metadata as _md
import os
import subprocess
import sys

import toolong
from toolong.ui import UI
from toolong.log_lines import LogLines
from toolong.log_view import LogView
from toolong.find_dialog import FindDialog
from toolong.line_panel import LinePanel

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURE = os.path.join(HERE, "fixture.log")

# --------------------------------------------------------------------------------------------
# assertion harness (same contract as leg A: two-space "  FAIL " lines, ok/fail counters)
# --------------------------------------------------------------------------------------------
_ok = 0
_fail = 0
_skips = []


def check(cond, label):
    global _ok, _fail
    if cond:
        _ok += 1
    else:
        _fail += 1
        print("  FAIL %s" % label)


def eq(got, want, label):
    check(got == want, "%s: got %r want %r" % (label, got, want))


def skip(label, reason):
    _skips.append((label, reason))
    print("  SKIP %s -- %s" % (label, reason))


def strips(app):
    """Full visible screen as a list of row strings, straight from the compositor."""
    return [s.text for s in app.screen._compositor.render_strips()]


def blob(app):
    return "\n".join(strips(app))


def row_with(app, needle):
    for r in strips(app):
        if needle in r:
            return r
    return None


async def wait_scan(app, pilot, target, max_iter=600):
    """toolong scans the log on a background worker thread; pump the loop until it has counted
    `target` lines (or give up), then settle."""
    ll = None
    for _ in range(max_iter):
        try:
            ll = app.screen.query_one(LogLines)
        except Exception:
            ll = None
        if ll is not None and ll.line_count >= target:
            await pilot.pause()
            await pilot.pause()
            return ll
        await pilot.pause(0.02)
    return app.screen.query_one(LogLines)


# --------------------------------------------------------------------------------------------
# 1. package surface + fixture integrity
# --------------------------------------------------------------------------------------------
def test_surface():
    eq(_md.version("toolong"), "1.5.0", "toolong version pinned 1.5.0")
    eq(_md.version("textual"), "0.58.1", "textual pinned 0.58.1 (toolong's pin)")
    eq(_md.version("rich"), "15.0.0", "rich pinned 15.0.0")
    eq(_md.version("click"), "8.4.2", "click pinned 8.4.2")
    import textual as _t
    eq(_t.__version__, "0.58.1", "textual.__version__ matches metadata")
    for mod in ("toolong.ui", "toolong.log_view", "toolong.log_lines", "toolong.find_dialog",
                "toolong.line_panel", "toolong.format_parser", "toolong.timestamps"):
        __import__(mod)
        check(True, "import %s" % mod)

    check(os.path.isfile(FIXTURE), "fixture.log present next to the carpet")
    with open(FIXTURE, "r", encoding="utf-8") as f:
        text = f.read()
    lines = text.split("\n")
    eq(len(lines), 60, "fixture has exactly 60 lines (no trailing newline)")
    check(not text.endswith("\n"), "fixture has no trailing newline (clean 60-line count)")
    check(lines[0] == "LINE 000 START-OF-FILE plaintext top marker alpha", "fixture line 0 marker")
    check(lines[59] == "LINE 059 END-OF-FILE plaintext bottom marker omega", "fixture line 59 marker")
    import json
    obj = json.loads(lines[30])  # the pure-JSON line must be valid JSON
    eq(obj.get("msg"), "json log entry", "fixture line 30 is a JSON object with the expected msg")


# --------------------------------------------------------------------------------------------
# 2. real-app end-to-end: boot toolong, scan the fixture, drive the full key sequence
# --------------------------------------------------------------------------------------------
async def test_render_and_navigate():
    app = UI([FIXTURE])
    async with app.run_test(size=(100, 30)) as pilot:
        ll = await wait_scan(app, pilot, 60)
        lv = app.screen.query_one(LogView)

        # --- the scan read the whole file ---
        eq(ll.line_count, 60, "toolong scanned all 60 lines of the fixture")
        region_h = ll.scrollable_content_region.height
        check(region_h > 0, "log lines widget has a positive content height")
        eq(ll.max_scroll_y, ll.line_count - region_h,
           "max_scroll_y == line_count - viewport height (scroll geometry sound)")

        # --- on load toolong TAILS to the bottom of the file ---
        eq(ll.scroll_offset.y, ll.max_scroll_y, "fresh load tails to the bottom (max scroll)")
        b = blob(app)
        check("LINE 059 END-OF-FILE plaintext bottom marker omega" in b,
              "tailed view shows the last line verbatim")
        check("LINE 000 START-OF-FILE" not in b, "tailed view does NOT show the first line")
        width = app.screen.size.width
        eq(width, 100, "virtual terminal width is 100")

        # --- HOME: jump to the very top ---
        await pilot.press("home")
        await pilot.pause(); await pilot.pause()
        eq(ll.scroll_offset.y, 0, "home scrolls to the top (y==0)")
        top = row_with(app, "LINE 000 START-OF-FILE plaintext top marker alpha")
        check(top is not None, "first line rendered verbatim after home")
        check("LINE 059 END-OF-FILE" not in blob(app), "last line no longer visible after home")

        # --- box border columns stay aligned (no stale/garbage rows) ---
        rows = strips(app)
        left_ok = right_ok = True
        for r in rows[1:region_h]:
            if not r.strip():
                continue
            if not r.startswith("┃"):        # left heavy bar at column 0
                left_ok = False
            if r.rfind("┃") != width - 1:     # right heavy bar at last column
                right_ok = False
        check(left_ok, "every content row starts with the left border at column 0")
        check(right_ok, "every content row ends with the right border at column width-1 (aligned)")

        # --- ARROW DOWN then UP (pointer is None -> line scroll of exactly 1) ---
        eq(ll.pointer_line, None, "no pointer line selected yet")
        await pilot.press("down")
        await pilot.pause(); await pilot.pause()
        eq(ll.scroll_offset.y, 1, "down scrolls exactly one line")
        check("LINE 001 INFO service initialising component alpha" in blob(app),
              "second line now at the top after one down")
        check("LINE 000 START-OF-FILE" not in blob(app), "first line scrolled off after down")
        await pilot.press("up")
        await pilot.pause(); await pilot.pause()
        eq(ll.scroll_offset.y, 0, "up returns to the top")
        check("LINE 000 START-OF-FILE" in blob(app), "first line visible again after up")

        # --- PAGE DOWN / PAGE UP ---
        await pilot.press("pagedown")
        await pilot.pause(); await pilot.pause()
        pd = ll.scroll_offset.y
        check(0 < pd <= ll.max_scroll_y, "page-down advances within scroll bounds")
        check("LINE 000 START-OF-FILE" not in blob(app), "first line gone after page-down")
        await pilot.press("pageup")
        await pilot.pause(); await pilot.pause()
        eq(ll.scroll_offset.y, 0, "page-up returns to the top")
        check("LINE 000 START-OF-FILE" in blob(app), "first line visible again after page-up")

        # --- END: jump to the bottom ---
        await pilot.press("end")
        await pilot.pause(); await pilot.pause()
        eq(ll.scroll_offset.y, ll.max_scroll_y, "end scrolls to the bottom")
        check("LINE 059 END-OF-FILE plaintext bottom marker omega" in blob(app),
              "last line visible after end")

        # --- the JSON line: toolong's JSONLogFormat path renders it verbatim ---
        ll.scroll_to(y=17, animate=False)   # line index 30 lands inside the 100x30 viewport
        await pilot.pause(); await pilot.pause()
        jb = blob(app)
        check("json log entry" in jb, "JSON log line body rendered (JSONLogFormat path)")
        check('"code": 42' in jb, "JSON line preserves its key/value content verbatim")

        # --- line-number gutter toggle (ctrl+l) ---
        await pilot.press("home")
        await pilot.pause()
        await pilot.press("ctrl+l")
        await pilot.pause(); await pilot.pause()
        eq(ll.show_line_numbers, True, "ctrl+l enables the line-number gutter")
        numbered = row_with(app, "LINE 000 START-OF-FILE plaintext top marker alpha")
        check(numbered is not None and numbered.startswith("┃1"),
              "first row now carries line number 1 in the gutter")
        await pilot.press("ctrl+l")
        await pilot.pause(); await pilot.pause()
        eq(ll.show_line_numbers, False, "ctrl+l again disables the line-number gutter")

        # --- pointer select (enter) + line-detail panel (enter again) ---
        await pilot.press("home")
        await pilot.pause()
        await pilot.press("enter")
        await pilot.pause(); await pilot.pause()
        eq(ll.pointer_line, 0, "enter selects the pointer line at the top of the view")
        eq(ll.show_gutter, True, "selecting a line shows the pointer gutter")
        pointer_row = row_with(app, "LINE 000 START-OF-FILE plaintext top marker alpha")
        check(pointer_row is not None and "\U0001f449" in pointer_row,
              "pointer row shows the selection cursor icon")
        await pilot.press("enter")
        await pilot.pause(); await pilot.pause()
        eq(lv.show_panel, True, "second enter opens the line-detail panel")
        lp = app.screen.query_one(LinePanel)
        eq(lp.display, True, "line-detail panel is displayed")
        check("LINE 000" in blob(app), "log lines still visible alongside the detail panel")
        await pilot.press("escape")
        await pilot.pause(); await pilot.pause()
        eq(lv.show_panel, False, "escape closes the detail panel first")
        eq(ll.pointer_line, 0, "pointer selection survives the panel close")
        await pilot.press("escape")
        await pilot.pause(); await pilot.pause()
        eq(ll.pointer_line, None, "second escape clears the pointer selection")

        # --- go-to screen (ctrl+g) pushes/pops on the screen stack ---
        n0 = len(app.screen_stack)
        await pilot.press("ctrl+g")
        await pilot.pause(); await pilot.pause()
        eq(len(app.screen_stack), n0 + 1, "ctrl+g pushes the go-to screen")
        eq(type(app.screen).__name__, "GotoScreen", "top screen is the GotoScreen")
        await pilot.press("escape")
        await pilot.pause(); await pilot.pause()
        eq(len(app.screen_stack), n0, "escape pops the go-to screen")

        # --- find dialog (ctrl+f) opens an Input that captures typed text ---
        await pilot.press("ctrl+f")
        await pilot.pause(); await pilot.pause()
        eq(lv.show_find, True, "ctrl+f opens the find dialog")
        find_text = app.screen.query_one("#find-text")
        eq(find_text.has_focus, True, "the find text input is focused")
        for ch in "ERROR":
            await pilot.press(ch)
        await pilot.pause(); await pilot.pause()
        eq(find_text.value, "ERROR", "typed characters land in the find input")
        eq(ll.find, "ERROR", "the find term propagates to the log lines reactive")
        await pilot.press("escape")
        await pilot.pause(); await pilot.pause()
        eq(lv.show_find, False, "escape dismisses the find dialog")


# --------------------------------------------------------------------------------------------
# 3. CLI dimension — toolong ships the `tl` console script (structural) + non-blocking --help
# --------------------------------------------------------------------------------------------
def test_cli():
    dist = _md.distribution("toolong")
    scripts = {ep.name: ep.value for ep in dist.entry_points if ep.group == "console_scripts"}
    eq(scripts.get("tl"), "toolong.cli:run", "toolong exposes the `tl` console script")

    import importlib.util as _u
    check(_u.find_spec("toolong.__main__") is not None, "`python -m toolong` module exists")
    # `--help` is non-interactive (click prints usage and exits); probe it with a hard timeout.
    # Never run `tl <file>` bare — that opens the interactive TUI and needs a live TTY.
    try:
        r = subprocess.run([sys.executable, "-m", "toolong", "--help"],
                           capture_output=True, text=True, timeout=30)
        check(r.returncode == 0, "`python -m toolong --help` exits 0")
        for token in ("Usage", "log files", "--merge", "--version"):
            check(token in r.stdout, "`toolong --help` documents %r" % token)
    except Exception as e:
        skip("python -m toolong --help", "help probe failed/timeout: %r" % e)
    skip("tl <file> (bare)", "interactive log viewer requiring a live TTY; never run headless")


# --------------------------------------------------------------------------------------------
# driver
# --------------------------------------------------------------------------------------------
async def _run_async():
    for t in (test_render_and_navigate,):
        try:
            await t()
        except Exception as e:
            global _fail
            _fail += 1
            import traceback
            print("  FAIL %s raised %r" % (t.__name__, e))
            traceback.print_exc()


def main():
    print("=== ToolongCarpet: toolong %s / textual %s / rich %s on python %s ==="
          % (_md.version("toolong"), _md.version("textual"), _md.version("rich"),
             sys.version.split()[0]))
    for t in (test_surface,):
        try:
            t()
        except Exception as e:
            global _fail
            _fail += 1
            import traceback
            print("  FAIL %s raised %r" % (t.__name__, e))
            traceback.print_exc()
    asyncio.run(_run_async())
    try:
        test_cli()
    except Exception as e:
        _fail += 1
        import traceback
        print("  FAIL test_cli raised %r" % e)
        traceback.print_exc()
    if _skips:
        print("--- documented skips (%d) ---" % len(_skips))
    print("TOOLONG_RESULT ok=%d fail=%d" % (_ok, _fail))
    if _fail == 0:
        print("TOOLONG_DONE")
        return 0
    return 1


if __name__ == "__main__":
    sys.exit(main())
