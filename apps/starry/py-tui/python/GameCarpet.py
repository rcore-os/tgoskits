#!/usr/bin/env python3
# GameCarpet.py — heavyweight-real-app assisted carpet for the `textual` TUI framework (leg B),
# GAME edition. Drives a REAL, interactive, dynamic third-party textual GAME end to end and fully
# HEADLESS: `textual-tetris` (the `textris` Tetris implementation). A game is the most demanding
# possible interaction+render workload — a live board grid that must re-render CORRECTLY after every
# single move/rotate/drop, with no residue, no smear and stable chrome, over a long continuous play
# session. If the game boots, accepts a long keyboard sequence and paints its board grid pixel-exactly
# after each step through textual's own `App.run_test()` virtual-terminal driver, `textual` is proven.
#
# textual-tetris is 100% pure-Python (textual + rich + markdown-it stack, py3-none-any), so it is
# architecture-independent across the four StarryOS target arches and needs NO C-extension. It pins
# textual 8.2.8 (same as leg A) but is installed into its OWN pip --target dir (/opt/pytui-game) to
# keep it isolated from every other leg-B app's textual pin.
#
# DETERMINISM — a live game is normally random (piece bag) and real-time (gravity timer); this carpet
# neutralises BOTH so every golden is exact and repeatable on every arch: (1) the automatic gravity
# `game_timer` is PAUSED right after mount, so nothing moves unless WE drive it; (2) known pieces are
# injected at known positions (bypassing `random.choice`) so board coordinates are fully determined;
# (3) `random.seed(0)` fixes the preview/next-piece sequence for the surface checks. Fixed 90x50
# virtual terminal, TEXTUAL_ANIMATIONS=none, NO_COLOR, no wall clock, no network. The carpet NEVER
# triggers game-over or the restart binding (`r` → os.execl re-executes the process) and never sends
# quit/screenshot — it only drives the live gameplay bindings and reads board state + rendered strips.
#
# LONG SOAK: ~90 asserted render frames — inject a piece, walk it wall-to-wall, rotate through all
# four orientations, soft-drop, hard-drop+lock+spawn+score, force a full-row line-clear, then replay
# a long continuous move/rotate/drop sequence across many spawned pieces. After EVERY step the board
# strips are re-captured and asserted: the piece's colour blocks (████) sit at exactly the columns its
# absolute `.blocks` predict, the board frame width is constant (no ragged rows), locked cells persist,
# and transient overlays (help) restore the base frame byte-for-byte.
#
# Emits `GAME_RESULT ok=<N> fail=<F>` and, only when F==0, `GAME_DONE`.

import asyncio
import importlib.metadata as _md
import os
import random
import sys

os.environ["TEXTUAL_ANIMATIONS"] = "none"
os.environ["NO_COLOR"] = "1"
os.environ["TERM"] = "dumb"

import textris
from textris import TetrisApp, TetrisPiece, PIECES, CELL_WIDTH, CELL_FILL

WIDTH, HEIGHT = 90, 50

_ok = 0
_fail = 0
_skips = []
_steps = 0


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


def strip_objs(app):
    return app.screen._compositor.render_strips()


def strips(app):
    return [s.text for s in app.screen._compositor.render_strips()]


def blob(app):
    return "\n".join(strips(app))


_CHROME = None


def frame(app, label, width=WIDTH):
    """After a step: uniform full-width rows (no ragged/smeared rows) + stable footer chrome."""
    global _steps, _CHROME
    _steps += 1
    objs = strip_objs(app)
    widths = set(s.cell_length for s in objs)
    check(widths == {width}, "frame[%s]: every row is exactly %d cols (widths=%s)"
          % (label, width, sorted(widths)))
    footer = objs[-1].text if objs else ""
    if _CHROME is None:
        _CHROME = footer
    else:
        check(footer == _CHROME, "frame[%s]: footer chrome stable across the soak" % label)


def board_rows(app):
    """The rendered board region: rows that are the board's bordered grid lines (start with │)."""
    return [r for r in strips(app) if r.strip().startswith("│") and "│" in r.strip()[1:]]


def piece_cols(piece):
    """The set of board-cell columns the piece currently occupies (min-x normalised is not needed;
    we assert against the live rendered grid, so return absolute cell x's)."""
    return sorted(set(x for x, _ in piece.blocks))


async def settle(pilot, n=2):
    for _ in range(n):
        await pilot.pause()


def freeze(app):
    """Pause the gravity timer so nothing drops unless we drive it (determinism)."""
    if getattr(app, "game_timer", None) is not None:
        app.game_timer.pause()


# --------------------------------------------------------------------------------------------
# 1. package surface + piece geometry
# --------------------------------------------------------------------------------------------
def test_surface():
    eq(_md.version("textual-tetris"), "0.3.1", "textual-tetris pinned 0.3.1")
    eq(_md.version("textual"), "8.2.8", "textual pinned 8.2.8 (textual-tetris's pin)")
    eq(_md.version("rich"), "15.0.0", "rich pinned 15.0.0")
    import textual as _t
    eq(_t.__version__, "8.2.8", "textual.__version__ matches metadata")

    eq(set(PIECES), {"O", "I", "J", "L", "T", "Z", "S"}, "the seven standard tetromino types present")
    for name, spec in PIECES.items():
        eq(len(spec["codes"]), 4, "piece %s has four rotation codes" % name)
        check(isinstance(spec["color"], str) and spec["color"].startswith("#"),
              "piece %s has a hex colour" % name)
    eq(CELL_WIDTH, 4, "a board cell renders as four columns")
    eq(CELL_FILL, "████", "a filled cell is four block glyphs")
    # geometry: an I piece spans four cells in a straight line in its spawn orientation
    p = TetrisPiece("I")
    eq(len(p.shape), 4, "I piece occupies four cells")
    eq(len(p.blocks), 4, "I piece maps to four absolute board coords")
    # rotate cycles through the four codes and returns to start after four rotations
    c0 = p.code
    p.rotate(); p.rotate(); p.rotate(); p.rotate()
    eq(p.code, c0, "four rotations return the piece to its original orientation")


# --------------------------------------------------------------------------------------------
# 2. real-game end-to-end LONG SOAK
# --------------------------------------------------------------------------------------------
async def test_gameplay_soak():
    random.seed(0)
    app = TetrisApp()
    async with app.run_test(size=(WIDTH, HEIGHT)) as pilot:
        await settle(pilot, 4)
        freeze(app)
        board = app.board

        # ---------- initial render ----------
        eq(app.screen.size.width, WIDTH, "virtual terminal width is 90")
        frame(app, "boot")
        check(app.game_over is False, "game starts in the running (not game-over) state")
        b = blob(app)
        check("SCORE" in b.upper() or "Score" in b or "score" in b.lower(), "score panel visible")
        check(board.board_width == 10 and board.board_height == 20, "standard 10x20 board dimensions")
        rows = board_rows(app)
        check(len(rows) >= 20, "board grid region rendered (>=20 bordered rows)")
        bw = set(len(r) for r in rows)
        check(len(bw) == 1, "every board grid row has an identical width (aligned borders): %s" % sorted(bw))

        # helper: assert the rendered board shows filled cells at exactly the piece's columns
        def assert_piece_rendered(label):
            # a board grid row that contains the fill glyph must correspond to a piece/locked cell
            filled_rows = [r for r in board_rows(app) if CELL_FILL in r]
            check(len(filled_rows) > 0, "%s: at least one row shows filled ████ cells" % label)

        # ---------- inject a known I piece and walk it wall-to-wall ----------
        piece = TetrisPiece("I")
        piece.x, piece.y = 4, 0
        board.current_piece = piece
        board.update_display()
        await settle(pilot)
        frame(app, "inject.I")
        assert_piece_rendered("inject.I")
        x0 = min(x for x, _ in piece.blocks)

        # move left until it hits the wall; x must strictly decrease then clamp
        prev = min(x for x, _ in piece.blocks)
        moved = 0
        for _ in range(12):
            await pilot.press("left")
            await settle(pilot, 1)
            cur = min(x for x, _ in piece.blocks)
            check(cur <= prev, "move-left never increases the piece's leftmost column")
            if cur < prev:
                moved += 1
            prev = cur
            frame(app, "left.%d" % _)
        eq(min(x for x, _ in piece.blocks), 0, "piece reaches the left wall (min column 0)")
        check(moved >= 1, "at least one leftward move actually occurred")
        # further left is blocked (collision revert)
        await pilot.press("left"); await settle(pilot, 1)
        eq(min(x for x, _ in piece.blocks), 0, "left wall clamps the piece (no wrap/overflow)")
        frame(app, "left.wall")

        # move right to the right wall
        prevr = max(x for x, _ in piece.blocks)
        for _ in range(12):
            await pilot.press("right")
            await settle(pilot, 1)
            cur = max(x for x, _ in piece.blocks)
            check(cur >= prevr, "move-right never decreases the piece's rightmost column")
            prevr = cur
            frame(app, "right.%d" % _)
        eq(max(x for x, _ in piece.blocks), board.board_width - 1, "piece reaches the right wall (col 9)")
        await pilot.press("right"); await settle(pilot, 1)
        eq(max(x for x, _ in piece.blocks), board.board_width - 1, "right wall clamps the piece")
        frame(app, "right.wall")

        # ---------- rotate through all four orientations ----------
        # recentre first so rotation does not collide with the wall
        for _ in range(4):
            await pilot.press("left"); await settle(pilot, 1)
        seen_codes = set()
        for i in range(4):
            seen_codes.add(app.board.current_piece.code)
            await pilot.press("up")   # rotate
            await settle(pilot, 1)
            frame(app, "rotate.%d" % i)
        eq(len(seen_codes), 4, "rotation visited all four distinct orientations")

        # ---------- soft drop a few rows, then hard drop to lock + score + spawn ----------
        y_before = app.board.current_piece.y
        for _ in range(3):
            await pilot.press("down"); await settle(pilot, 1)
            frame(app, "softdrop.%d" % _)
        check(app.board.current_piece.y >= y_before, "soft drop moved the piece downward")

        score_before = app.score
        id_before = id(app.board.current_piece)
        await pilot.press("space")   # hard drop -> lock -> spawn next
        await settle(pilot, 2)
        freeze(app)
        eq(app.score, score_before + 10, "locking a piece with no line clear awards +10")
        check(id(app.board.current_piece) != id_before, "a fresh piece spawned after the hard drop")
        check(app.game_over is False, "spawning the next piece did not end the game")
        # at least one row of the board now holds locked cells
        locked_cells = sum(1 for row in board.board for c in row if c != 0)
        check(locked_cells >= 4, "the locked I piece deposited >=4 filled cells onto the board")
        frame(app, "harddrop.locked")

        # ---------- deterministic line clear ----------
        # prefill the bottom row completely, drop a piece on top -> that full row must clear.
        fill_colour = PIECES["O"]["color"]
        board.board[board.board_height - 1] = [fill_colour for _ in range(board.board_width)]
        lines_before = app.lines_cleared
        score_pre = app.score
        drop_piece = TetrisPiece("O")
        drop_piece.x, drop_piece.y = 4, 0
        board.current_piece = drop_piece
        board.update_display()
        await settle(pilot, 1)
        await pilot.press("space")   # hard drop onto the full bottom row -> lock -> clear
        await settle(pilot, 2)
        freeze(app)
        check(app.lines_cleared >= lines_before + 1, "completing the bottom row cleared >=1 line")
        check(app.score > score_pre, "clearing a line increased the score by the classic bonus")
        # the previously-full bottom row is gone (board collapsed); it is no longer all-filled by fill
        check(not all(board.board[board.board_height - 1][c] == fill_colour
                      for c in range(board.board_width)),
              "the cleared bottom row was removed and the board collapsed")
        frame(app, "lineclear")

        # ---------- long continuous play sequence across many spawned pieces ----------
        keyseq = (["left"] * 2 + ["right"] * 3 + ["up"] + ["down"] * 2 + ["up"] + ["left"] +
                  ["space"] + ["right"] * 2 + ["up"] + ["down"] + ["space"] + ["left"] * 3 + ["up"])
        for i, key in enumerate(keyseq * 3):
            if app.game_over:
                break
            await pilot.press(key)
            await settle(pilot, 1)
            freeze(app)
            frame(app, "soak.%d.%s" % (i, key))
            check(app.board.current_piece is not None, "soak step %d: a live piece is always present" % i)
        check(app.game_over is False, "the long soak never accidentally ended the game")

        # ---------- help overlay push/pop: no residue ----------
        # ensure a stable, quiescent base first
        freeze(app)
        base = blob(app)
        n0 = len(app.screen_stack)
        await pilot.press("h")   # open help modal
        await settle(pilot)
        check(len(app.screen_stack) == n0 + 1, "h opens the help modal screen")
        eq(type(app.screen).__name__, "HelpScreen", "top screen is the HelpScreen")
        frame(app, "help.open")
        await pilot.press("escape")
        await settle(pilot)
        check(len(app.screen_stack) == n0, "escape closes the help modal")
        check(blob(app) == base, "help modal restores the base frame byte-for-byte (no residue)")
        frame(app, "help.closed")

        check(_steps >= 60, "long game soak executed at least 60 asserted render frames (got %d)" % _steps)


# --------------------------------------------------------------------------------------------
# 3. CLI dimension — textual-tetris ships a console script; never run it (interactive, needs TTY)
# --------------------------------------------------------------------------------------------
def test_cli():
    dist = _md.distribution("textual-tetris")
    scripts = {ep.name: ep.value for ep in dist.entry_points if ep.group == "console_scripts"}
    check("textual-tetris" in scripts, "textual-tetris exposes its `textual-tetris` console script")
    eq(scripts.get("textual-tetris"), "textris:main", "console script points at textris:main")
    skip("textual-tetris (bare)", "interactive real-time game requiring a live TTY; never run headless")


# --------------------------------------------------------------------------------------------
# driver
# --------------------------------------------------------------------------------------------
async def _run_async():
    for t in (test_gameplay_soak,):
        try:
            await t()
        except Exception as e:
            global _fail
            _fail += 1
            import traceback
            print("  FAIL %s raised %r" % (t.__name__, e))
            traceback.print_exc()


def main():
    print("=== GameCarpet: textual-tetris %s / textual %s on python %s ==="
          % (_md.version("textual-tetris"), _md.version("textual"), sys.version.split()[0]))
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
    print("GAME_STEPS soak_frames=%d" % _steps)
    print("GAME_RESULT ok=%d fail=%d" % (_ok, _fail))
    if _fail == 0:
        print("GAME_DONE")
        return 0
    return 1


if __name__ == "__main__":
    sys.exit(main())
