#!/usr/bin/env python3
# run_pytui.py — on-target gate for the StarryOS Python TUI carpet (textual + casca).
#
# Runs each framework carpet (TextualCarpet / CascaCarpet) as its own subprocess; each carpet
# self-checks and prints `<LIB>_RESULT ok=N fail=F`, then `<LIB>_DONE` only when its fail count is
# zero. A carpet counts as OK only when its DONE marker is present, it exited 0, and no `  FAIL `
# line appears in its output. Prints `PY_TUI_OK=<P>/<T>` and `TEST PASSED` only when BOTH carpets
# pass, else `TEST FAILED`. The PASS/FAIL anchor lives ONLY here so the success regex cannot
# self-match on the launch command.
#
# Deterministic and headless: textual runs through App.run_test() (virtual terminal, no TTY);
# casca renders through an in-process capture surface. No threads, timers, network, or randomness.
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
os.chdir(HERE)

# Keep textual/rich from probing a real terminal; force a plain, deterministic environment.
os.environ.setdefault("PYTHONDONTWRITEBYTECODE", "1")
os.environ.setdefault("PYTHONUNBUFFERED", "1")
os.environ.setdefault("TERM", "dumb")
os.environ.setdefault("NO_COLOR", "1")
os.environ.setdefault("COLUMNS", "80")
os.environ.setdefault("LINES", "24")

PY = sys.executable
# (name, module, DONE marker, PYTHONPATH for its subprocess). Each real-app carpet imports from its
# OWN pip --target dir because the pins are MUTUALLY INCOMPATIBLE: leg A (textual 8.2.8), toolong
# (0.58.1), frogmouth (0.43.2), posting (6.1.0) and tetris (8.2.8) cannot share a site-packages.
CARPETS = [
    ("textual", "TextualCarpet.py", "TEXTUAL_DONE", "/opt/pytui"),
    ("casca", "CascaCarpet.py", "CASCA_DONE", "/opt/pytui"),
    ("toolong", "ToolongCarpet.py", "TOOLONG_DONE", "/opt/pytui-toolong"),
    ("frogmouth", "FrogmouthCarpet.py", "FROGMOUTH_DONE", "/opt/pytui-frogmouth"),
    # leg B (heavy): a real interactive textual game + the heaviest real textual API user.
    # posting runs last: it is by far the heaviest carpet (giant DOM), so ordering it last keeps
    # the peak resource footprint at the very end of the run.
    ("tetris", "GameCarpet.py", "GAME_DONE", "/opt/pytui-game"),
    ("posting", "PostingCarpet.py", "POSTING_DONE", "/opt/pytui-posting"),
]

print("=== python3 %s ===" % sys.version.split()[0])
print("=== py-tui: TUI framework carpets (textual | casca) + real apps (toolong | frogmouth | posting | tetris) — headless, exact-assertion ===")

passed = 0
for name, fn, marker, pythonpath in CARPETS:
    env = dict(os.environ)
    env["PYTHONPATH"] = pythonpath  # isolate each real app's conflicting textual pin
    r = subprocess.run([PY, os.path.join(HERE, fn)], capture_output=True, text=True, env=env)
    out = (r.stdout or "") + (r.stderr or "")
    result = [ln for ln in out.splitlines() if "_RESULT ok=" in ln]
    if marker in out and r.returncode == 0 and "  FAIL " not in out:
        print("  OK   %s (%s)" % (name, result[0].strip() if result else ""))
        passed += 1
    else:
        print("  FAIL %s (%s) rc=%s" % (name, marker, r.returncode))
        bad = [ln for ln in out.splitlines()
               if any(t in ln for t in ("FAIL", "Error", "Traceback", "Exception"))]
        print("\n".join(bad[-20:]))

total = len(CARPETS)
print("PY_TUI_RESULT PASS=%d TOTAL=%d" % (passed, total))
print("PY_TUI_OK=%d/%d" % (passed, total))
if passed == total and total > 0:
    print("TEST PASSED")
    sys.exit(0)
print("TEST FAILED")
sys.exit(1)
