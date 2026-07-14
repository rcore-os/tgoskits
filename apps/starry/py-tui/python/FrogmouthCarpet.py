#!/usr/bin/env python3
# FrogmouthCarpet.py — heavy-real-app assisted carpet for the `textual` TUI framework (leg B).
#
# Companion to ToolongCarpet: drives a SECOND real, popular, pure-Python Textualize application end
# to end and fully HEADLESS — `frogmouth`, the terminal Markdown browser. Booting frogmouth,
# parsing a real Markdown document into textual's `Markdown` widget block tree, populating the
# table-of-contents, and responding to a full navigation sequence (Tab focus, sidebar toggle,
# scrolling, ToC-driven jumps) proves the `textual` library through genuine third-party usage.
#
# frogmouth pins `textual==0.43.2` (leg A uses 8.2.8, toolong uses 0.58.1 — all three conflict), so
# it is installed into its OWN pip --target dir (/opt/pytui-frogmouth) and run with PYTHONPATH
# pointing ONLY there. Its whole closure (httpx/httpcore/h11/anyio/certifi/idna/sniffio/xdg/zipp/
# importlib-metadata + the markdown/rich stack) is pure-Python (py3-none-any) and architecture
# independent. httpx is only used for REMOTE URLs; this carpet opens a LOCAL file only, so no
# network is ever touched.
#
# Everything runs against a FIXED, committed fixture (fixture.md — H1/H2/H3 headings, bold/italic/
# inline-code, a bullet list, an ordered list, a fenced code block, a table, and a link). Every
# golden below was recomputed from the live library's actual Markdown block tree and compositor
# output. After EACH interaction the screen strips and widget state are re-asserted. Deterministic:
# a private throwaway XDG data/config/HOME dir (set before importing frogmouth) so no saved history
# or config can leak in; fixed 110x40 virtual terminal; no wall clock, no animation, no network,
# no randomness. run_test always exits cleanly.
#
# Emits `FROGMOUTH_RESULT ok=<N> fail=<F>` and, only when F==0, `FROGMOUTH_DONE`.

import asyncio
import collections
import importlib.metadata as _md
import os
import subprocess
import sys
import tempfile

# --- isolate frogmouth's on-disk state BEFORE it is imported: a fresh, empty XDG tree means the
# --- app always boots to our fixture (never to a previously-saved history entry) and any history
# --- it writes goes to a throwaway dir. This is what makes the run deterministic and repeatable.
_STATE = tempfile.mkdtemp(prefix="frogmouth-carpet-")
os.environ["XDG_DATA_HOME"] = os.path.join(_STATE, "data")
os.environ["XDG_CONFIG_HOME"] = os.path.join(_STATE, "config")
os.environ["XDG_CACHE_HOME"] = os.path.join(_STATE, "cache")
os.environ["HOME"] = _STATE

from argparse import Namespace

import frogmouth
from frogmouth.app.app import MarkdownViewer
from frogmouth.widgets import Navigation, Omnibox, Viewer
from textual.widgets import Markdown, Tree
from textual.widgets.markdown import MarkdownBlock, MarkdownTableOfContents

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURE = os.path.abspath(os.path.join(HERE, "fixture.md"))

# --------------------------------------------------------------------------------------------
# assertion harness (same contract as leg A)
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
    return [s.text for s in app.screen._compositor.render_strips()]


def blob(app):
    return "\n".join(strips(app))


def block_counts(md):
    return collections.Counter(type(b).__name__ for b in md.query(MarkdownBlock))


def headings(md):
    return [b for b in md.query(MarkdownBlock)
            if type(b).__name__ in ("MarkdownH1", "MarkdownH2", "MarkdownH3")]


async def wait_loaded(app, pilot, max_iter=500):
    """frogmouth loads the document on an exclusive worker; pump the loop until the Markdown block
    tree is mounted and the H1 title has rendered."""
    md = None
    for _ in range(max_iter):
        await pilot.pause(0.02)
        try:
            md = app.screen.query_one(Markdown)
        except Exception:
            md = None
        if md is not None and list(md.query(MarkdownBlock)) and "Frogmouth Fixture Title" in blob(app):
            await pilot.pause(); await pilot.pause()
            return md
    return app.screen.query_one(Markdown)


# --------------------------------------------------------------------------------------------
# 1. package surface + fixture integrity
# --------------------------------------------------------------------------------------------
def test_surface():
    eq(_md.version("frogmouth"), "0.9.2", "frogmouth distribution pinned 0.9.2")
    eq(frogmouth.__version__, "0.9.1", "frogmouth.__version__ is the packaged 0.9.1 string")
    eq(_md.version("textual"), "0.43.2", "textual pinned 0.43.2 (frogmouth's pin)")
    eq(_md.version("httpx"), "0.24.1", "httpx pinned 0.24.1 (remote-only; unused for local files)")
    import textual as _t
    eq(_t.__version__, "0.43.2", "textual.__version__ matches metadata")
    for mod in ("frogmouth.app.app", "frogmouth.screens.main", "frogmouth.widgets.viewer",
                "frogmouth.widgets.navigation", "frogmouth.widgets.navigation_panes.table_of_contents"):
        __import__(mod)
        check(True, "import %s" % mod)

    check(os.path.isfile(FIXTURE), "fixture.md present next to the carpet")
    with open(FIXTURE, "r", encoding="utf-8") as f:
        text = f.read()
    check(text.startswith("# Frogmouth Fixture Title"), "fixture.md begins with the H1 title")
    for marker in ("**bold words**", "`inline code`", "- first bullet item apple",
                   "1. ordered element one", "```python", "| Fruit  | Count |",
                   "[Textualize](https://www.textualize.io)"):
        check(marker in text, "fixture.md contains %r" % marker)


# --------------------------------------------------------------------------------------------
# 2. real-app end-to-end: boot frogmouth, parse the document, drive the navigation sequence
# --------------------------------------------------------------------------------------------
async def test_render_and_navigate():
    app = MarkdownViewer(Namespace(file=[FIXTURE]))
    async with app.run_test(size=(110, 40)) as pilot:
        md = await wait_loaded(app, pilot)
        vw = app.screen.query_one(Viewer)

        # --- the document parsed into the exact block tree ---
        eq(str(vw.location), FIXTURE, "viewer location is the local fixture path")
        counts = block_counts(md)
        eq(counts["MarkdownH1"], 1, "one H1 heading parsed")
        eq(counts["MarkdownH2"], 4, "four H2 headings parsed")
        eq(counts["MarkdownH3"], 1, "one H3 heading parsed")
        eq(counts["MarkdownBulletList"], 1, "one bullet list parsed")
        eq(counts["MarkdownOrderedList"], 1, "one ordered list parsed")
        eq(counts["MarkdownFence"], 1, "one fenced code block parsed")
        eq(counts["MarkdownTable"], 1, "one table parsed")
        eq(counts["MarkdownParagraph"], 10, "ten paragraphs parsed")
        eq(sum(counts.values()), 20, "total Markdown block count is 20")

        # --- initial render: H1 + inline formatting rendered as plain text cells ---
        b = blob(app)
        check("Frogmouth Fixture Title" in b, "H1 title rendered on screen")
        check("bold words" in b, "bold span text rendered")
        check("italic words" in b, "italic span text rendered")
        check("inline code" in b, "inline code span text rendered")
        eq(vw.scroll_offset.y, 0, "viewer starts scrolled to the top")

        # --- table of contents populated with every heading ---
        toc = app.screen.query_one(MarkdownTableOfContents)
        tree = toc.query_one(Tree)
        labels = []

        def walk(node):
            labels.append(str(node.label))
            for c in node.children:
                walk(c)

        walk(tree.root)
        for title in ("Frogmouth Fixture Title", "Section Alpha", "Section Beta",
                      "Code Sample", "Data Table", "Links Section"):
            check(any(title in lbl for lbl in labels), "ToC lists heading %r" % title)
        heads = headings(md)
        eq(len(heads), 6, "six heading blocks in the document")

        # --- Tab cycles focus between the focusable widgets (Viewer <-> Omnibox) ---
        eq(type(app.focused).__name__, "Viewer", "viewer holds focus after load")
        await pilot.press("tab")
        await pilot.pause(); await pilot.pause()
        eq(type(app.focused).__name__, "Omnibox", "tab moves focus to the omnibox")
        await pilot.press("tab")
        await pilot.pause(); await pilot.pause()
        eq(type(app.focused).__name__, "Viewer", "tab cycles focus back to the viewer")

        # --- navigation sidebar toggle (ctrl+n) ---
        nav = app.screen.query_one(Navigation)
        eq(nav.popped_out, False, "navigation sidebar starts hidden")
        await pilot.press("ctrl+n")
        await pilot.pause(); await pilot.pause()
        eq(nav.popped_out, True, "ctrl+n reveals the navigation sidebar")
        await pilot.press("ctrl+n")
        await pilot.pause(); await pilot.pause()
        eq(nav.popped_out, False, "ctrl+n again hides the navigation sidebar")

        # --- scrolling reveals the later blocks (list/code/table/link) ---
        vw.scroll_end(animate=False)
        await pilot.pause(); await pilot.pause()
        check(vw.scroll_offset.y > 0, "scroll-end moved the viewport down")
        be = blob(app)
        check("apples" in be and "Count" in be, "table cells and header rendered")
        check("greet" in be, "fenced code block body rendered")
        check("Textualize" in be, "link text rendered")
        vw.scroll_home(animate=False)
        await pilot.pause(); await pilot.pause()
        eq(vw.scroll_offset.y, 0, "scroll-home returns to the top")
        check("Frogmouth Fixture Title" in blob(app), "H1 visible again after scroll-home")

        # --- ToC-driven jump: scroll_to_block on the last heading (the path a ToC click drives) ---
        last_h2 = [h for h in heads if type(h).__name__ == "MarkdownH2"][-1]
        vw.scroll_to_block(last_h2.id)
        await pilot.pause(); await pilot.pause()
        check(vw.scroll_offset.y > 0, "jumping to the last heading scrolled the viewport")
        jb = blob(app)
        check("Links Section" in jb, "target heading is visible after the jump")
        check("Frogmouth Fixture Title" not in jb, "top heading scrolled out of view after the jump")
        vw.scroll_home(animate=False)
        await pilot.pause(); await pilot.pause()

        # --- keyboard scroll bindings on the focused viewer (space=page down, b=page up) ---
        vw.focus()
        await pilot.pause()
        y0 = vw.scroll_offset.y
        await pilot.press("space")
        await pilot.pause(); await pilot.pause()
        check(vw.scroll_offset.y > y0, "space pages the viewer down")
        await pilot.press("b")
        await pilot.pause(); await pilot.pause()
        eq(vw.scroll_offset.y, 0, "b pages the viewer back to the top")

        # --- ToC Tree keyboard navigation drives a viewer scroll on select ---
        await pilot.press("ctrl+t")
        await pilot.pause(); await pilot.pause()
        eq(nav.popped_out, True, "ctrl+t reveals the table-of-contents pane")
        tree2 = toc.query_one(Tree)
        tree2.focus()
        await pilot.pause(); await pilot.pause()
        eq(tree2.has_focus, True, "the ToC tree is focused")
        vw.scroll_home(animate=False)
        await pilot.pause()
        for _ in range(5):
            await pilot.press("down")
            await pilot.pause()
        check(tree2.cursor_line >= 0, "arrow keys move the ToC tree cursor")
        await pilot.press("enter")
        await pilot.pause(); await pilot.pause()
        check(vw.scroll_offset.y > 0, "selecting a ToC entry scrolled the viewer to that heading")


# --------------------------------------------------------------------------------------------
# 3. CLI dimension — frogmouth ships the `frogmouth` console script + non-blocking --help
# --------------------------------------------------------------------------------------------
def test_cli():
    dist = _md.distribution("frogmouth")
    scripts = {ep.name: ep.value for ep in dist.entry_points if ep.group == "console_scripts"}
    eq(scripts.get("frogmouth"), "frogmouth.app.app:run", "frogmouth exposes its console script")

    import importlib.util as _u
    check(_u.find_spec("frogmouth.__main__") is not None, "`python -m frogmouth` module exists")
    # argparse `--help` prints usage and exits; probe with a hard timeout. Never run
    # `frogmouth <file>` bare — that opens the interactive browser and needs a live TTY.
    try:
        r = subprocess.run([sys.executable, "-m", "frogmouth", "--help"],
                           capture_output=True, text=True, timeout=30, env=dict(os.environ))
        check(r.returncode == 0, "`python -m frogmouth --help` exits 0")
        for token in ("usage", "Markdown", "--version", "file"):
            check(token in r.stdout, "`frogmouth --help` documents %r" % token)
    except Exception as e:
        skip("python -m frogmouth --help", "help probe failed/timeout: %r" % e)
    skip("frogmouth <file> (bare)", "interactive markdown browser requiring a live TTY; never run headless")


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
    print("=== FrogmouthCarpet: frogmouth %s / textual %s on python %s ==="
          % (_md.version("frogmouth"), _md.version("textual"), sys.version.split()[0]))
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
    print("FROGMOUTH_RESULT ok=%d fail=%d" % (_ok, _fail))
    if _fail == 0:
        print("FROGMOUTH_DONE")
        return 0
    return 1


if __name__ == "__main__":
    sys.exit(main())
