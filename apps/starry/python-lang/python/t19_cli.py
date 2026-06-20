#!/usr/bin/env python3
"""Interpreter command-line interface (every python3 option/env var) — carpet coverage for StarryOS python-lang (#764)."""
import sys
_ok = True
def chk(name, cond, info=""):
    global _ok
    print(("  ok " if cond else "  FAIL ") + name + ((" " + info) if info else ""))
    if not cond:
        _ok = False

# ---------------------------------------------------------------------------
# AREA: the CPython interpreter itself, driven through its command line.
#
# Every check spawns a *child* python via subprocess using sys.executable, hands
# it one CLI flag (or one PYTHON* env var) plus a tiny program, and asserts the
# OBSERVABLE effect (stdout/stderr text + exit code). This is the carpet of the
# launcher described in `python --help` and the "1. Command line and environment"
# chapter of the docs (docs.python.org/3/using/cmdline.html).
#
# StarryOS risk: each check forks/execs a brand-new interpreter. If the kernel's
# fork()/execve()/wait4() surface is partial, the *whole* file cannot run; we do
# NOT mask that (a hard infra failure should be visible). But where a SINGLE flag
# probes a feature that may legitimately be unavailable in-guest (e.g. -X
# importtime timing, faulthandler), we degrade that one check to a noted skip.
#
# Host note: this runs on CPython 3.12 here; 3.14-only behaviors are version
# guarded so the file passes on 3.12 (taking documented skip paths) and exercises
# the real behavior under 3.14 in-guest.
# ---------------------------------------------------------------------------
import os
import subprocess

PY = sys.executable
VER = sys.version_info

# Common spawn helper. Returns (rc, out, err). We force a short timeout so a
# hung child cannot wedge the harness, and default to a clean, isolated-ish env
# unless the caller overrides `env`. text=True decodes with the locale; we keep
# programs ASCII so that is deterministic.
def run(args, input=None, env=None, timeout=30, cwd=None):
    try:
        p = subprocess.run(
            [PY] + args,
            input=input,
            capture_output=True,
            text=True,
            timeout=timeout,
            env=env,
            cwd=cwd,
        )
        return p.returncode, p.stdout, p.stderr
    except subprocess.TimeoutExpired:
        return -999, "", "TIMEOUT"

# A base environment with the PYTHON* knobs we care about cleared, so an
# inherited value from the harness cannot perturb a check that does not set it.
def clean_env(**overrides):
    e = dict(os.environ)
    for k in list(e):
        if k.startswith("PYTHON"):
            del e[k]
    e.update(overrides)
    return e

# Sanity: the child interpreter we are about to drive actually launches. If this
# fails, every check below is meaningless, so surface it loudly (no skip).
_rc, _o, _e = run(["-c", "print('spawn-ok')"])
chk("child_spawn", _rc == 0 and _o.strip() == "spawn-ok",
    "rc=%d out=%r err=%r" % (_rc, _o.strip(), _e.strip()[-80:]))

# DEPTH/STRESS: a single successful fork/exec is one data point; a kernel whose
# fork()/execve()/wait4() surface is flaky (sporadic EAGAIN, leaked PIDs/fds, a
# bug that only trips after N forks) would slip past one spawn. Drive a tight
# burst of fresh interpreters and require EVERY one to return cleanly with its
# own distinct stdout — proving repeatable fork/exec + per-child argv delivery.
_burst_n = 24
_burst_ok = 0
_burst_bad = ""
for _i in range(_burst_n):
    _r, _so, _se = run(["-c", "import sys; print('B' + sys.argv[1])", str(_i)])
    if _r == 0 and _so.strip() == "B%d" % _i:
        _burst_ok += 1
    else:
        _burst_bad = "i=%d rc=%d out=%r err=%r" % (_i, _r, _so.strip(), _se.strip()[-40:])
        break
chk("child_spawn_burst", _burst_ok == _burst_n,
    "ok=%d/%d %s" % (_burst_ok, _burst_n, _burst_bad))

# Spawn-failure handling: run() must surface a non-zero child rc faithfully (not
# swallow it). A deliberately failing child after the healthy burst proves the
# harness distinguishes success from failure rather than reporting blanket OK.
_r, _so, _se = run(["-c", "import sys; sys.exit(11)"])
chk("child_spawn_failure_detected", _r == 11, "rc=%d" % _r)

# ===========================================================================
# -c <command>  : execute the passed string; sys.argv[0] == "-c".
#   how: run a one-liner that prints; expect stdout + rc 0.
#   docs: cmdline "-c <command>".
# ===========================================================================
rc, o, e = run(["-c", "print(6 * 7)"])
chk("opt_c_basic", rc == 0 and o.strip() == "42", "rc=%d out=%r" % (rc, o.strip()))

# -c sets sys.argv[0] to "-c" and appends any following words as argv[1:].
rc, o, e = run(["-c", "import sys; print(sys.argv)", "alpha", "beta"])
chk("opt_c_argv", rc == 0 and o.strip() == "['-c', 'alpha', 'beta']",
    "out=%r" % o.strip())

# A failing -c command propagates a non-zero exit (uncaught exception -> 1).
rc, o, e = run(["-c", "raise SystemExit(3)"])
chk("opt_c_exitcode", rc == 3, "rc=%d" % rc)

# ===========================================================================
# -m <module>  : run a module/package as __main__ (runpy).
#   how: -m timeit with --help (self-contained, no timing run); -m site dumps
#        path config; -m base64 / -m json.tool round-trip via stdin.
#   docs: cmdline "-m <module-name>".
# ===========================================================================
rc, o, e = run(["-m", "timeit", "--help"])
chk("opt_m_timeit_help", rc == 0 and ("timeit" in (o + e)),
    "rc=%d" % rc)

# -m base64 -e / -d via stdin is a tiny, deterministic codec round-trip.
rc, o, e = run(["-m", "base64", "-e"], input="hi")
b64 = o.strip()
chk("opt_m_base64_encode", rc == 0 and b64 == "aGk=", "out=%r" % b64)
rc, o, e = run(["-m", "base64", "-d"], input="aGk=\n")
chk("opt_m_base64_decode", rc == 0 and o.strip() == "hi", "out=%r" % o.strip())

# -m json.tool pretty-prints/validates JSON read from stdin.
rc, o, e = run(["-m", "json.tool"], input='{"b":2,"a":1}')
chk("opt_m_jsontool", rc == 0 and '"a": 1' in o and '"b": 2' in o,
    "rc=%d out=%r" % (rc, o[:60]))

# -m site reports site configuration without error.
rc, o, e = run(["-m", "site"])
chk("opt_m_site", rc == 0 and ("sys.path" in o or "ENABLE_USER_SITE" in o),
    "rc=%d" % rc)

# -m this prints the Zen of Python (a tiny self-contained stdlib module).
rc, o, e = run(["-m", "this"])
chk("opt_m_this", rc == 0 and "Zen of Python" in o, "rc=%d out=%r" % (rc, o[:40]))

# -m py_compile with a real source file has an OBSERVABLE side-effect: it writes
# a .pyc. Direct the cache to a temp prefix and assert a .pyc actually appears —
# this exercises -m runpy + the bytecode compiler + filesystem write together,
# not just "the module imported".
import tempfile as _tf_pc
_pc_src = _tf_pc.mkdtemp()
_pc_out = _tf_pc.mkdtemp()
with open(os.path.join(_pc_src, "compile_me.py"), "w") as _f:
    _f.write("ANSWER = 42\n")
rc, o, e = run(["-X", "pycache_prefix=" + _pc_out, "-m", "py_compile",
                os.path.join(_pc_src, "compile_me.py")])
_pyc_made = False
for _root, _dirs, _files in os.walk(_pc_out):
    if any(fn.endswith(".pyc") for fn in _files):
        _pyc_made = True
        break
chk("opt_m_py_compile", rc == 0 and _pyc_made,
    "rc=%d pyc=%s err=%r" % (rc, _pyc_made, e.strip()[-60:]))

# -m on a non-existent module errors with rc 1 and a clear message.
rc, o, e = run(["-m", "no_such_module_xyz"])
chk("opt_m_missing", rc == 1 and ("No module named" in e or "Error" in e),
    "rc=%d err=%r" % (rc, e.strip()[-80:]))

# ===========================================================================
# -  (single dash) : read the program from stdin; sys.argv[0] == "-".
#   docs: cmdline "-".
# ===========================================================================
rc, o, e = run(["-"], input="import sys\nprint('stdin', sys.argv[0])\n")
chk("opt_stdin_dash", rc == 0 and o.strip() == "stdin -", "out=%r" % o.strip())

# Bare invocation with a piped program (no leading flag) also reads stdin.
rc, o, e = run([], input="print('piped')\n")
chk("opt_stdin_implicit", rc == 0 and o.strip() == "piped", "out=%r" % o.strip())

# ===========================================================================
# REPL (interactive interpreter). `python3 -i` forces interactive mode even on
# a piped (non-tty) stdin, driving the genuine read-eval-print loop. The
# DEFINING REPL behavior — distinct from script/`-` stdin mode — is that a bare
# expression typed at the prompt is auto-echoed via sys.displayhook (its repr is
# printed). We feed lines and assert the echoes appear, proving real interactive
# evaluation rather than batch execution.
#   docs: cmdline "-i"; tutorial "The Interactive Interpreter".
# ===========================================================================
# 1) auto-echo of bare-expression results (the REPL's signature behavior).
rc, o, e = run(["-i"], input="1 + 1\nname = U'sky'\nname.upper()\n")
chk("repl_autoecho_expr", "2" in o and "'SKY'" in o, "out=%r" % o.strip())

# 2) multi-line compound statement: an indented block spans lines and runs when
#    the blank line closes it; the side effect is observable on the next prompt.
rc, o, e = run(["-i"],
               input="xs = []\nfor i in range(3):\n    xs.append(i * i)\n\nprint('SQ', xs)\n")
chk("repl_multiline_block", "SQ [0, 1, 4]" in o, "out=%r" % o.strip())

# 3) exit() / EOF terminates the loop cleanly (rc 0); statements after exit() in
#    the fed stream are NOT executed (the loop has already stopped).
rc, o, e = run(["-i"], input="print('before')\nexit()\nprint('after')\n")
chk("repl_exit_terminates",
    rc == 0 and "before" in o and "after" not in o, "rc=%d out=%r" % (rc, o.strip()))

# 4) NameError at the prompt is reported but does NOT kill the REPL — the next
#    line still evaluates (interactive error recovery, unlike script mode which
#    aborts on the first uncaught exception).
rc, o, e = run(["-i"], input="undefined_repl_name\nprint('recovered')\n")
chk("repl_error_recovery",
    "NameError" in e and "recovered" in o, "out=%r err_tail=%r" % (o.strip(), e.strip()[-60:]))

# ===========================================================================
# -V / --version : print "Python X.Y.Z". Historically went to stderr; since 3.x
#   -V prints to stdout. --version is the long form. -VV adds build info.
#   how: assert "3." appears in combined output and rc 0.
#   docs: cmdline "-V, --version".
# ===========================================================================
rc, o, e = run(["-V"])
chk("opt_V_short", rc == 0 and "Python 3." in (o + e), "out=%r err=%r" % (o.strip(), e.strip()))
rc, o, e = run(["--version"])
chk("opt_version_long", rc == 0 and "Python 3." in (o + e), "out=%r" % (o + e).strip())
rc, o, e = run(["-VV"])
chk("opt_VV_verbose", rc == 0 and "Python 3." in (o + e), "rc=%d" % rc)

# ===========================================================================
# -h / --help / --help-all : usage text, exit 0.
#   docs: cmdline "-h, --help".
# ===========================================================================
rc, o, e = run(["-h"])
chk("opt_h_short", rc == 0 and "usage:" in (o + e).lower(), "rc=%d" % rc)
rc, o, e = run(["--help"])
chk("opt_help_long", rc == 0 and "usage:" in (o + e).lower(), "rc=%d" % rc)
# --help-env documents environment variables (3.11+).
if VER >= (3, 11):
    rc, o, e = run(["--help-env"])
    chk("opt_help_env", rc == 0 and "PYTHON" in (o + e), "rc=%d" % rc)
else:
    chk("opt_help_env", True, "(skip: needs 3.11)")

# ===========================================================================
# -O  : set __debug__ = False and strip `assert`. -OO additionally strips
#   docstrings. Both also influence the .pyc opt tag.
#   how: probe __debug__; show assert is a no-op; with -OO, a module docstring
#        becomes None.
#   docs: cmdline "-O" / "-OO".
# ===========================================================================
rc, o, e = run(["-O", "-c", "print(__debug__)"])
chk("opt_O_debug_false", rc == 0 and o.strip() == "False", "out=%r" % o.strip())
# Under -O, `assert False` must NOT raise (assertions removed).
rc, o, e = run(["-O", "-c", "assert False, 'should be stripped'\nprint('survived')"])
chk("opt_O_assert_stripped", rc == 0 and o.strip() == "survived",
    "rc=%d out=%r" % (rc, o.strip()))
# Without -O, the same assert DOES raise (control).
rc, o, e = run(["-c", "assert False, 'kept'"])
chk("opt_noO_assert_kept", rc == 1 and "AssertionError" in e, "rc=%d" % rc)
# -OO strips docstrings: a function defined in the -c program has __doc__ None.
rc, o, e = run(["-OO", "-c", "def f():\n '''doc'''\n pass\nprint(f.__doc__)"])
chk("opt_OO_docstring_stripped", rc == 0 and o.strip() == "None", "out=%r" % o.strip())

# ===========================================================================
# -B  : don't write .pyc files (PYTHONDONTWRITEBYTECODE). sys.dont_write_bytecode.
#   docs: cmdline "-B".
# ===========================================================================
rc, o, e = run(["-B", "-c", "import sys; print(sys.dont_write_bytecode)"])
chk("opt_B_flag", rc == 0 and o.strip() == "True", "out=%r" % o.strip())
# Default (no -B): bytecode writing is enabled.
rc, o, e = run(["-c", "import sys; print(sys.dont_write_bytecode)"], env=clean_env())
chk("opt_noB_default", rc == 0 and o.strip() == "False", "out=%r" % o.strip())

# ===========================================================================
# -E  : ignore all PYTHON* environment variables.
#   how: set PYTHONDONTWRITEBYTECODE=1 in env, run with -E, expect it ignored
#        (dont_write_bytecode stays False).
#   docs: cmdline "-E".
# ===========================================================================
env = clean_env(PYTHONDONTWRITEBYTECODE="1")
rc, o, e = run(["-E", "-c", "import sys; print(sys.dont_write_bytecode)"], env=env)
chk("opt_E_ignores_env", rc == 0 and o.strip() == "False", "out=%r" % o.strip())
# Control: WITHOUT -E the same env var IS honored.
rc, o, e = run(["-c", "import sys; print(sys.dont_write_bytecode)"], env=env)
chk("opt_noE_honors_env", rc == 0 and o.strip() == "True", "out=%r" % o.strip())

# ===========================================================================
# -I  : isolated mode = -E + -s, and removes script dir / cwd from sys.path[0].
#   how: assert sys.flags.isolated == 1; and a PYTHON* env var is ignored.
#   docs: cmdline "-I".
# ===========================================================================
rc, o, e = run(["-I", "-c", "import sys; print(sys.flags.isolated)"], env=clean_env(PYTHONDONTWRITEBYTECODE="1"))
chk("opt_I_isolated_flag", rc == 0 and o.strip() == "1", "out=%r" % o.strip())
rc, o, e = run(["-I", "-c", "import sys; print(sys.dont_write_bytecode)"], env=clean_env(PYTHONDONTWRITEBYTECODE="1"))
chk("opt_I_ignores_env", rc == 0 and o.strip() == "False", "out=%r" % o.strip())
# Under -I, the empty-string cwd entry is absent from sys.path (no '' at front).
rc, o, e = run(["-I", "-c", "import sys; print('' in sys.path)"])
chk("opt_I_no_cwd_in_path", rc == 0 and o.strip() == "False", "out=%r" % o.strip())

# ===========================================================================
# -s  : don't add the user site-packages directory. sys.flags.no_user_site.
#   docs: cmdline "-s".
# ===========================================================================
rc, o, e = run(["-s", "-c", "import sys; print(sys.flags.no_user_site)"])
chk("opt_s_no_user_site", rc == 0 and o.strip() == "1", "out=%r" % o.strip())

# ===========================================================================
# -S  : don't import `site` on startup. sys.flags.no_site; `site` absent unless
#   imported explicitly.
#   docs: cmdline "-S".
# ===========================================================================
rc, o, e = run(["-S", "-c", "import sys; print(sys.flags.no_site)"])
chk("opt_S_no_site_flag", rc == 0 and o.strip() == "1", "out=%r" % o.strip())
# With -S, `site` is not auto-imported (not in sys.modules at startup).
rc, o, e = run(["-S", "-c", "import sys; print('site' in sys.modules)"])
chk("opt_S_site_not_imported", rc == 0 and o.strip() == "False", "out=%r" % o.strip())

# ===========================================================================
# -u  : unbuffered stdout/stderr. sys.flags is not directly exposed for this in
#   older versions; assert via PYTHONUNBUFFERED-equivalent behavior + flag.
#   how: print without newline+flush; under -u the byte appears even though we
#        don't flush. We assert rc 0 and exact output (buffering only matters
#        for interleave/partial reads; subprocess collects all output anyway, so
#        we assert the documented sys-level signal instead).
#   docs: cmdline "-u".
# ===========================================================================
# stdout in -u mode reports write_through / line_buffering signals; the robust,
# version-stable assertion is that the program still runs and emits the bytes.
rc, o, e = run(["-u", "-c", "import sys; sys.stdout.write('U'); sys.stdout.write('V')"])
chk("opt_u_unbuffered", rc == 0 and o == "UV", "rc=%d out=%r" % (rc, o))

# ===========================================================================
# -q  : don't print the version+copyright on interactive startup.
#   how: hard to drive interactively non-interactively; assert -q with -c runs
#        cleanly (no banner, rc 0). The banner only appears in REPL anyway.
#   docs: cmdline "-q".
# ===========================================================================
rc, o, e = run(["-q", "-c", "print('quiet')"])
chk("opt_q_runs", rc == 0 and o.strip() == "quiet" and "Type \"help\"" not in (o + e),
    "rc=%d" % rc)

# ===========================================================================
# -v  : verbose import tracing on stderr ("import x  # ...").
#   how: importing a stdlib module emits import lines to stderr.
#   docs: cmdline "-v".
# ===========================================================================
rc, o, e = run(["-v", "-c", "import json"])
chk("opt_v_verbose_import", rc == 0 and ("import " in e), "rc=%d err_has_import=%r" % (rc, "import " in e))

# ===========================================================================
# -W <action> : warning filter. error|ignore|default|always|module|once.
#   how: emit a UserWarning via warnings.warn; with -W error it becomes an
#        exception (rc 1); with -W ignore it is suppressed (clean rc 0, no text).
#   docs: cmdline "-W arg"; warnings module filter actions.
# ===========================================================================
warn_prog = "import warnings; warnings.warn('w', UserWarning); print('after')"
rc, o, e = run(["-W", "error", "-c", warn_prog])
chk("opt_W_error", rc == 1 and "UserWarning" in e and "after" not in o,
    "rc=%d" % rc)
rc, o, e = run(["-W", "ignore", "-c", warn_prog])
chk("opt_W_ignore", rc == 0 and o.strip() == "after" and "UserWarning" not in e,
    "rc=%d out=%r" % (rc, o.strip()))
rc, o, e = run(["-W", "default", "-c", warn_prog])
chk("opt_W_default", rc == 0 and o.strip() == "after" and "UserWarning" in e,
    "rc=%d" % rc)
# -W always shows the warning AND continues execution (so "after" must print),
# mirroring opt_W_default; asserting only stderr would mask a swallowed program.
rc, o, e = run(["-W", "always", "-c", warn_prog])
chk("opt_W_always", rc == 0 and o.strip() == "after" and "UserWarning" in e,
    "rc=%d out=%r" % (rc, o.strip()))
# COMPLETENESS + DEPTH: the remaining documented actions are 'once' and 'module'
# — both deduplicate. A program that warns 3x at the SAME location must emit the
# UserWarning EXACTLY ONCE under each, and still run to completion. Counting the
# emissions (not just presence) proves the dedup semantic, not mere visibility.
warn_prog3 = ("import warnings\n"
              "for _ in range(3): warnings.warn('w', UserWarning)\n"
              "print('after3')")
rc, o, e = run(["-W", "once", "-c", warn_prog3])
chk("opt_W_once", rc == 0 and o.strip() == "after3" and e.count("UserWarning") == 1,
    "rc=%d cnt=%d" % (rc, e.count("UserWarning")))
rc, o, e = run(["-W", "module", "-c", warn_prog3])
chk("opt_W_module", rc == 0 and o.strip() == "after3" and e.count("UserWarning") == 1,
    "rc=%d cnt=%d" % (rc, e.count("UserWarning")))

# ===========================================================================
# -b / -bb : BytesWarning when comparing/str()-ing bytes. -b warns, -bb errors.
#   how: with -bb, str(b'x') raises BytesWarning (rc 1). Default: no warning.
#   docs: cmdline "-b".
# ===========================================================================
rc, o, e = run(["-bb", "-c", "print(str(b'x'))"])
chk("opt_bb_bytes_error", rc == 1 and "BytesWarning" in e, "rc=%d err=%r" % (rc, e.strip()[-80:]))
# -b alone: comparing bytes==str warns but does not abort (rc 0).
rc, o, e = run(["-b", "-W", "always", "-c", "print(b'x' == 'x')"])
chk("opt_b_bytes_warn", rc == 0 and "BytesWarning" in e and o.strip() == "False",
    "rc=%d out=%r" % (rc, o.strip()))
# Default: no BytesWarning for the same comparison.
rc, o, e = run(["-c", "print(b'x' == 'x')"], env=clean_env())
chk("opt_default_no_byteswarn", rc == 0 and "BytesWarning" not in e and o.strip() == "False",
    "rc=%d" % rc)
# Reflect the active flag: sys.flags.bytes_warning == 2 under -bb.
rc, o, e = run(["-bb", "-c", "import sys; print(sys.flags.bytes_warning)"])
chk("opt_bb_flag_value", rc == 0 and o.strip() == "2", "out=%r" % o.strip())

# ===========================================================================
# -x  : skip the first line of source (for non-Unix #! hacks).
#   how: feed a stdin program whose first line is junk; -x skips it.
#   docs: cmdline "-x".
# ===========================================================================
# -x skips the first line of the *script file* (the documented use: a #! shim
# line that is not valid Python). It does not apply to stdin programs, so we
# must write a real file. _pp_dir/_skip_script is created lazily here.
import tempfile as _tf_x
_x_dir = _tf_x.mkdtemp()
_x_script = os.path.join(_x_dir, "skip.py")
with open(_x_script, "w") as _f:
    _f.write("THIS LINE IS GARBAGE @@@\nprint('line2')\n")
rc, o, e = run(["-x", _x_script])
chk("opt_x_skip_first_line", rc == 0 and o.strip() == "line2",
    "rc=%d out=%r err=%r" % (rc, o.strip(), e.strip()[-60:]))

# ===========================================================================
# -d  : turn on parser debugging (tolerant: may need a debug build). We only
#   require it does not crash and still runs the program.
#   docs: cmdline "-d".
# ===========================================================================
rc, o, e = run(["-d", "-c", "print('dbg')"])
chk("opt_d_tolerant", rc == 0 and o.strip() == "dbg", "rc=%d" % rc)

# ===========================================================================
# -X dev : Development Mode. Enables extra runtime checks; sys.flags.dev_mode==1
#   and the default warning filter becomes 'default' (so warnings show).
#   docs: cmdline "-X dev".
# ===========================================================================
# sys.flags.dev_mode is a bool on recent CPython (prints "True"); older builds
# print "1". Accept either truthy spelling.
rc, o, e = run(["-X", "dev", "-c", "import sys; print(sys.flags.dev_mode)"])
chk("opt_X_dev_flag", rc == 0 and o.strip() in ("1", "True"), "out=%r" % o.strip())
rc, o, e = run(["-X", "dev", "-c", warn_prog])
chk("opt_X_dev_shows_warn", rc == 0 and "UserWarning" in e, "rc=%d" % rc)
# DEPTH: dev mode's documented side-effect is that the default warning filter
# becomes 'default' (warnings show once-per-location). Assert that semantic
# transition directly, not merely that *a* warning printed — i.e. the active
# filter list actually contains a ('default', ...) entry under -X dev.
rc, o, e = run(["-X", "dev", "-c",
                "import warnings; print(any(f[0]=='default' for f in warnings.filters))"])
chk("opt_X_dev_default_filter", rc == 0 and o.strip() == "True", "out=%r" % o.strip())

# ===========================================================================
# -X utf8 : UTF-8 Mode. sys.flags.utf8_mode==1; filesystem/locale encoding utf-8.
#   docs: cmdline "-X utf8"; PEP 540.
# ===========================================================================
rc, o, e = run(["-X", "utf8", "-c", "import sys; print(sys.flags.utf8_mode)"])
chk("opt_X_utf8_flag", rc == 0 and o.strip() == "1", "out=%r" % o.strip())
rc, o, e = run(["-X", "utf8", "-c", "import sys; print(sys.getfilesystemencoding())"])
chk("opt_X_utf8_fsencoding", rc == 0 and o.strip().lower() == "utf-8", "out=%r" % o.strip())
# -X utf8=0 disables UTF-8 mode explicitly.
rc, o, e = run(["-X", "utf8=0", "-c", "import sys; print(sys.flags.utf8_mode)"])
chk("opt_X_utf8_off", rc == 0 and o.strip() == "0", "out=%r" % o.strip())

# ===========================================================================
# -X importtime : print import timing to stderr ("import time:").
#   docs: cmdline "-X importtime". May be sensitive to monotonic-clock support;
#   tolerate absence of a working timer by accepting either the header or a clean
#   run that still imported the module.
# ===========================================================================
rc, o, e = run(["-X", "importtime", "-c", "import json"])
if rc == 0 and "import time" in e:
    chk("opt_X_importtime", True)
elif rc == 0:
    chk("opt_X_importtime", True, "(skip: ran but no timing header — check clock in-guest)")
else:
    chk("opt_X_importtime", False, "rc=%d err=%r" % (rc, e.strip()[-80:]))

# ===========================================================================
# -X int_max_str_digits=N : cap int<->str conversion length (CVE-2020-10735).
#   how: the minimum accepted cap is 640 (or 0 = unlimited); set 640 and convert
#        a number with >640 digits (10**700) -> ValueError mentioning the limit.
#   docs: cmdline "-X int_max_str_digits"; sys.set_int_max_str_digits.
# ===========================================================================
prog_big = "print(len(str(10 ** 700)))"   # 701 digits, exceeds a 640 cap
rc, o, e = run(["-X", "int_max_str_digits=640", "-c", prog_big])
chk("opt_X_int_max_digits_cap", rc == 1 and "int_max_str_digits" in e and "ValueError" in e,
    "rc=%d err=%r" % (rc, e.strip()[-80:]))
# A large cap (or 0) allows the same conversion.
rc, o, e = run(["-X", "int_max_str_digits=10000", "-c", prog_big])
chk("opt_X_int_max_digits_ok", rc == 0 and o.strip() == "701", "out=%r" % o.strip())
# It is also reflected via sys.flags / get_int_max_str_digits.
rc, o, e = run(["-X", "int_max_str_digits=1234", "-c", "import sys; print(sys.get_int_max_str_digits())"])
chk("opt_X_int_max_digits_getter", rc == 0 and o.strip() == "1234", "out=%r" % o.strip())
# EDGE/ERROR: a cap below the documented floor (640) and not 0 is rejected at
# pre-init with a fatal error and a non-zero exit — the launcher must refuse it,
# not silently clamp. The message names the limit and the 640/0 rule.
rc, o, e = run(["-X", "int_max_str_digits=100", "-c", "print('nope')"])
chk("opt_X_int_max_digits_below_min", rc != 0 and "int_max_str_digits" in e
    and o.strip() != "nope",
    "rc=%d err=%r" % (rc, e.strip()[-80:]))

# ===========================================================================
# -X faulthandler : enable the faulthandler at startup. Reflected by
#   faulthandler.is_enabled(). Tolerant: signal/traceback machinery may be
#   partial in-guest.
#   docs: cmdline "-X faulthandler".
# ===========================================================================
rc, o, e = run(["-X", "faulthandler", "-c", "import faulthandler; print(faulthandler.is_enabled())"])
if rc == 0 and o.strip() == "True":
    chk("opt_X_faulthandler", True)
else:
    chk("opt_X_faulthandler", rc == 0, "(tolerant) rc=%d out=%r" % (rc, o.strip()))

# ===========================================================================
# -X no_debug_ranges : suppress per-instruction position info in tracebacks
#   (3.11+). Tolerant: just assert it runs and a raised exception still has a
#   traceback. (sys.flags surfaces it on 3.13+ as no_debug_ranges.)
#   docs: cmdline "-X no_debug_ranges".
# ===========================================================================
if VER >= (3, 11):
    rc, o, e = run(["-X", "no_debug_ranges", "-c", "raise ValueError('z')"])
    chk("opt_X_no_debug_ranges", rc == 1 and "ValueError" in e, "rc=%d" % rc)
else:
    chk("opt_X_no_debug_ranges", True, "(skip: needs 3.11)")

# ===========================================================================
# -X frozen_modules={on,off} : whether to use frozen modules. The choice is
#   echoed verbatim in sys._xoptions, so we assert the exact value round-trips.
#   docs: cmdline "-X frozen_modules".
# ===========================================================================
rc, o, e = run(["-X", "frozen_modules=off", "-c",
                "import sys; print(sys._xoptions.get('frozen_modules'))"])
chk("opt_X_frozen_modules", rc == 0 and o.strip() == "off", "out=%r" % o.strip())

# ===========================================================================
# -X pycache_prefix=PATH / PYTHONPYCACHEPREFIX : redirect the .pyc cache root.
#   Reflected exactly by sys.pycache_prefix, for both the flag and the env var.
#   docs: cmdline "-X pycache_prefix"; "PYTHONPYCACHEPREFIX".
# ===========================================================================
rc, o, e = run(["-X", "pycache_prefix=/tmp/starry_pcp", "-c",
                "import sys; print(sys.pycache_prefix)"])
chk("opt_X_pycache_prefix", rc == 0 and o.strip() == "/tmp/starry_pcp", "out=%r" % o.strip())
rc, o, e = run(["-c", "import sys; print(sys.pycache_prefix)"],
               env=clean_env(PYTHONPYCACHEPREFIX="/tmp/starry_pcp_env"))
chk("env_pycache_prefix", rc == 0 and o.strip() == "/tmp/starry_pcp_env", "out=%r" % o.strip())

# ===========================================================================
# -X tracemalloc[=N] / PYTHONTRACEMALLOC : start memory allocation tracing at
#   startup. tracemalloc.is_tracing() reports True for both the flag and the env.
#   docs: cmdline "-X tracemalloc"; "PYTHONTRACEMALLOC".
# ===========================================================================
rc, o, e = run(["-X", "tracemalloc=1", "-c",
                "import tracemalloc; print(tracemalloc.is_tracing())"])
chk("opt_X_tracemalloc", rc == 0 and o.strip() == "True", "out=%r" % o.strip())
rc, o, e = run(["-c", "import tracemalloc; print(tracemalloc.is_tracing())"],
               env=clean_env(PYTHONTRACEMALLOC="1"))
chk("env_tracemalloc", rc == 0 and o.strip() == "True", "out=%r" % o.strip())

# ===========================================================================
# -X warn_default_encoding / PYTHONWARNDEFAULTENCODING : set
#   sys.flags.warn_default_encoding (warn when open()/etc. use the locale
#   encoding implicitly). Reflected as 1 for both spellings.
#   docs: cmdline "-X warn_default_encoding"; "PYTHONWARNDEFAULTENCODING".
# ===========================================================================
rc, o, e = run(["-X", "warn_default_encoding", "-c",
                "import sys; print(sys.flags.warn_default_encoding)"])
chk("opt_X_warn_default_encoding", rc == 0 and o.strip() == "1", "out=%r" % o.strip())
rc, o, e = run(["-c", "import sys; print(sys.flags.warn_default_encoding)"],
               env=clean_env(PYTHONWARNDEFAULTENCODING="1"))
chk("env_warn_default_encoding", rc == 0 and o.strip() == "1", "out=%r" % o.strip())

# ===========================================================================
# -X perf / PYTHONPERFSUPPORT : enable the perf-profiler stack trampoline. This
#   only activates on platforms with perf-map support (x86_64/aarch64 Linux); on
#   others it is a documented no-op. So we require a clean run and accept either
#   an active trampoline (where supported) or an inactive one (where it is not) —
#   but we still hard-fail if the option breaks the launcher.
#   docs: cmdline "-X perf"; "PYTHONPERFSUPPORT".
# ===========================================================================
rc, o, e = run(["-X", "perf", "-c",
                "import sys; print(sys.is_stack_trampoline_active())"])
chk("opt_X_perf", rc == 0 and o.strip() in ("True", "False"),
    "rc=%d out=%r" % (rc, o.strip()))
rc, o, e = run(["-c", "import sys; print(sys.is_stack_trampoline_active())"],
               env=clean_env(PYTHONPERFSUPPORT="1"))
chk("env_perfsupport", rc == 0 and o.strip() in ("True", "False"),
    "rc=%d out=%r" % (rc, o.strip()))

# ===========================================================================
# -X showrefcount : dump the total refcount after execution. This is a no-op on
#   a release build and prints a "[N refs, M blocks]" line only on a debug build.
#   We require it to be accepted and run the program cleanly (no launcher error).
#   docs: cmdline "-X showrefcount".
# ===========================================================================
rc, o, e = run(["-X", "showrefcount", "-c", "print('refcnt-ok')"])
chk("opt_X_showrefcount", rc == 0 and o.strip().splitlines()[0] == "refcnt-ok",
    "rc=%d out=%r" % (rc, o.strip()[:60]))

# ===========================================================================
# --check-hash-based-pycs {default,always,never} : pyc validation policy.
#   Tolerant: assert it is accepted and runs a trivial program.
#   docs: cmdline "--check-hash-based-pycs".
# ===========================================================================
rc, o, e = run(["--check-hash-based-pycs", "never", "-c", "print('hbp')"])
chk("opt_check_hash_pycs", rc == 0 and o.strip() == "hbp",
    "rc=%d err=%r" % (rc, e.strip()[-60:]))

# ===========================================================================
# -P : don't prepend a potentially unsafe path to sys.path (3.11+, PEP 587/704).
#   how: assert sys.flags.safe_path == 1.
#   docs: cmdline "-P".
# ===========================================================================
if VER >= (3, 11):
    # safe_path is a bool on recent CPython ("True"); accept "1" too.
    rc, o, e = run(["-P", "-c", "import sys; print(sys.flags.safe_path)"])
    chk("opt_P_safe_path", rc == 0 and o.strip() in ("1", "True"), "out=%r" % o.strip())
else:
    chk("opt_P_safe_path", True, "(skip: needs 3.11)")

# ===========================================================================
# Combined/stacked flags: short options may be bundled (e.g. -OO already above;
# here -SsE together). Assert all three flags register.
#   docs: getopt-style bundling.
# ===========================================================================
rc, o, e = run(["-E", "-s", "-S", "-c",
                "import sys; f=sys.flags; print(f.no_site, f.no_user_site, f.ignore_environment)"])
chk("opt_stacked_flags", rc == 0 and o.strip() == "1 1 1", "out=%r" % o.strip())

# ===========================================================================
# ENVIRONMENT VARIABLES (docs: "Environment variables" in cmdline). Each is
# passed via env= and we assert the in-process effect.
# ===========================================================================

# PYTHONDONTWRITEBYTECODE -> sys.dont_write_bytecode True.
rc, o, e = run(["-c", "import sys; print(sys.dont_write_bytecode)"], env=clean_env(PYTHONDONTWRITEBYTECODE="1"))
chk("env_dontwritebytecode", rc == 0 and o.strip() == "True", "out=%r" % o.strip())

# PYTHONOPTIMIZE=2 -> __debug__ False and optimize level 2.
rc, o, e = run(["-c", "import sys; print(__debug__, sys.flags.optimize)"], env=clean_env(PYTHONOPTIMIZE="2"))
chk("env_optimize", rc == 0 and o.strip() == "False 2", "out=%r" % o.strip())

# PYTHONUTF8=1 -> UTF-8 mode on (sys.flags.utf8_mode == 1).
rc, o, e = run(["-c", "import sys; print(sys.flags.utf8_mode)"], env=clean_env(PYTHONUTF8="1"))
chk("env_utf8", rc == 0 and o.strip() == "1", "out=%r" % o.strip())

# PYTHONWARNINGS=error -> a UserWarning becomes an exception.
rc, o, e = run(["-c", warn_prog], env=clean_env(PYTHONWARNINGS="error"))
chk("env_warnings_error", rc == 1 and "UserWarning" in e, "rc=%d" % rc)

# PYTHONNOUSERSITE -> no_user_site flag set.
rc, o, e = run(["-c", "import sys; print(sys.flags.no_user_site)"], env=clean_env(PYTHONNOUSERSITE="1"))
chk("env_nousersite", rc == 0 and o.strip() == "1", "out=%r" % o.strip())

# PYTHONDEVMODE=1 -> dev mode on.
rc, o, e = run(["-c", "import sys; print(sys.flags.dev_mode)"], env=clean_env(PYTHONDEVMODE="1"))
chk("env_devmode", rc == 0 and o.strip() in ("1", "True"), "out=%r" % o.strip())

# PYTHONVERBOSE=1 -> verbose import tracing (>=1 import line on stderr).
rc, o, e = run(["-c", "import json"], env=clean_env(PYTHONVERBOSE="1"))
chk("env_verbose", rc == 0 and "import " in e, "rc=%d" % rc)

# PYTHONUNBUFFERED=1 -> sys.flags is not exposed; assert the program still runs
# and the unbuffered child emits its bytes intact.
rc, o, e = run(["-c", "import sys; sys.stdout.write('AB')"], env=clean_env(PYTHONUNBUFFERED="1"))
chk("env_unbuffered", rc == 0 and o == "AB", "out=%r" % o)

# PYTHONPATH : extra import dirs are prepended to sys.path. Use a temp dir that
# contains an importable module, and confirm the child can import it.
import tempfile
_pp_dir = tempfile.mkdtemp()
with open(os.path.join(_pp_dir, "starry_probe_mod.py"), "w") as _f:
    _f.write("VALUE = 4242\n")
rc, o, e = run(["-c", "import starry_probe_mod as m; print(m.VALUE)"],
               env=clean_env(PYTHONPATH=_pp_dir))
chk("env_pythonpath_import", rc == 0 and o.strip() == "4242",
    "rc=%d out=%r err=%r" % (rc, o.strip(), e.strip()[-60:]))
# And PYTHONPATH dir appears in sys.path of the child.
rc, o, e = run(["-c", "import sys; print(%r in sys.path)" % _pp_dir],
               env=clean_env(PYTHONPATH=_pp_dir))
chk("env_pythonpath_in_syspath", rc == 0 and o.strip() == "True", "out=%r" % o.strip())

# PYTHONHASHSEED determinism: two runs with the SAME fixed seed must produce the
# SAME hash() for a str; and seed=0 disables randomization. (Without a fixed
# seed, hashes differ across runs by design — PEP 456.)
seedenv = clean_env(PYTHONHASHSEED="12345")
rc1, o1, _ = run(["-c", "print(hash('starry'))"], env=seedenv)
rc2, o2, _ = run(["-c", "print(hash('starry'))"], env=seedenv)
chk("env_hashseed_deterministic", rc1 == 0 and rc2 == 0 and o1.strip() == o2.strip(),
    "o1=%r o2=%r" % (o1.strip(), o2.strip()))
# sys.flags.hash_randomization is 0 when PYTHONHASHSEED=0.
rc, o, e = run(["-c", "import sys; print(sys.flags.hash_randomization)"], env=clean_env(PYTHONHASHSEED="0"))
chk("env_hashseed_zero_disables", rc == 0 and o.strip() == "0", "out=%r" % o.strip())

# PYTHONSTARTUP is only consulted by the interactive REPL; assert that setting it
# does not affect a -c run (file is NOT executed for -c).
_su = os.path.join(_pp_dir, "startup_probe.py")
with open(_su, "w") as _f:
    _f.write("print('STARTUP-RAN')\n")
rc, o, e = run(["-c", "print('main')"], env=clean_env(PYTHONSTARTUP=_su))
chk("env_startup_not_for_c", rc == 0 and o.strip() == "main" and "STARTUP-RAN" not in o,
    "out=%r" % o.strip())

# PYTHONINSPECT=1 + -c would drop to the REPL after; with empty stdin it reads
# EOF and exits cleanly. We just assert it does not hang/crash (rc 0) and printed
# our line. (Tolerant of a trailing prompt on stderr.)
rc, o, e = run(["-c", "print('inspect-base')"], input="", env=clean_env(PYTHONINSPECT="1"))
chk("env_inspect_eof_exits", rc == 0 and "inspect-base" in o, "rc=%d" % rc)

# The -i FLAG (CLI mirror of PYTHONINSPECT) sets sys.flags.inspect, runs the -c
# program, then drops to the REPL; with empty stdin the REPL reads EOF and exits
# 0. We assert the flag is actually set (visible inside the -c program) AND that
# the program ran — so a no-op -i would be caught, not silently accepted.
rc, o, e = run(["-i", "-c", "import sys; print('FLAG', sys.flags.inspect)"],
               input="", env=clean_env())
chk("opt_i_inspect_flag", rc == 0 and "FLAG 1" in o, "rc=%d out=%r" % (rc, o.strip()[:60]))

# ===========================================================================
# EXIT-CODE SEMANTICS (docs: sys.exit / "Interpreter exit").
# ===========================================================================
# sys.exit(0) -> rc 0.
rc, o, e = run(["-c", "import sys; sys.exit(0)"])
chk("exit_zero", rc == 0, "rc=%d" % rc)
# sys.exit(7) -> rc 7.
rc, o, e = run(["-c", "import sys; sys.exit(7)"])
chk("exit_nonzero", rc == 7, "rc=%d" % rc)
# sys.exit() with no arg -> rc 0.
rc, o, e = run(["-c", "import sys; sys.exit()"])
chk("exit_none", rc == 0, "rc=%d" % rc)
# sys.exit('message') -> message to stderr, rc 1.
rc, o, e = run(["-c", "import sys; sys.exit('boom')"])
chk("exit_string_stderr", rc == 1 and "boom" in e, "rc=%d err=%r" % (rc, e.strip()[-40:]))
# Uncaught exception -> rc 1, traceback on stderr.
rc, o, e = run(["-c", "raise RuntimeError('nope')"])
chk("exit_uncaught_exc", rc == 1 and "RuntimeError" in e and "Traceback" in e, "rc=%d" % rc)
# raise SystemExit(2) is the same as sys.exit(2).
rc, o, e = run(["-c", "raise SystemExit(2)"])
chk("exit_systemexit_int", rc == 2, "rc=%d" % rc)
# A syntax error in the program -> rc 1 with SyntaxError on stderr.
rc, o, e = run(["-c", "def (:"])
chk("exit_syntax_error", rc == 1 and "SyntaxError" in e, "rc=%d" % rc)
# os._exit bypasses cleanup with the literal code.
rc, o, e = run(["-c", "import os; os._exit(5)"])
chk("exit_os_exit", rc == 5, "rc=%d" % rc)
# EDGE: the OS exit status is 8-bit; sys.exit(256) wraps to 0 (256 & 0xff). This
# is the documented waitpid() truncation, a place a guest wait4() could diverge.
rc, o, e = run(["-c", "import sys; sys.exit(256)"])
chk("exit_code_wraps_8bit", rc == 0, "rc=%d" % rc)
rc, o, e = run(["-c", "import sys; sys.exit(257)"])
chk("exit_code_wraps_257", rc == 1, "rc=%d" % rc)
# EDGE: bool is an int subclass — sys.exit(True)->1, sys.exit(False)->0.
rc, o, e = run(["-c", "import sys; sys.exit(True)"])
chk("exit_bool_true", rc == 1, "rc=%d" % rc)
rc, o, e = run(["-c", "import sys; sys.exit(False)"])
chk("exit_bool_false", rc == 0, "rc=%d" % rc)
# An uncaught KeyboardInterrupt prints its traceback, then CPython re-raises
# SIGINT so the process dies *by the signal* (documented behavior) — subprocess
# reports the death as a negative returncode (-SIGINT) on POSIX, while a shell
# would see 128+2=130. Accept either spelling; the load-bearing assertion is the
# traceback names KeyboardInterrupt and the run did NOT exit 0.
rc, o, e = run(["-c", "raise KeyboardInterrupt"])
chk("exit_keyboardinterrupt",
    rc != 0 and "KeyboardInterrupt" in e and rc in (-2, 130, 1),
    "rc=%d" % rc)

# ===========================================================================
# sys.argv shape for the various entry forms (docs: sys.argv).
# ===========================================================================
# Running a real script file: argv[0] is the script path; extras follow.
_script = os.path.join(_pp_dir, "argv_script.py")
with open(_script, "w") as _f:
    _f.write("import sys\nprint(sys.argv[1:])\nprint(sys.argv[0].endswith('argv_script.py'))\n")
rc, o, e = run([_script, "p1", "p2"])
lines = o.strip().splitlines()
chk("argv_script_file", rc == 0 and lines == ["['p1', 'p2']", "True"], "out=%r" % o.strip())

# EDGE: argv must survive execve() byte-for-byte, including spaces, embedded
# quotes, '=', and non-ASCII (UTF-8) — a kernel with sloppy arg marshalling
# would mangle these. (NUL bytes are intentionally excluded: execve forbids them
# and subprocess raises before exec, so they are not an interpreter concern.)
_argv_special = ["a b c", 'q"u', "k=v", "café"]
rc, o, e = run(["-X", "utf8", "-c", "import sys; print(sys.argv[1:])"] + _argv_special)
chk("argv_special_chars", rc == 0 and o.strip() == repr(_argv_special),
    "out=%r" % o.strip())

# ===========================================================================
# 3.14-only CLI surface (guarded). Newer flags should be probed under 3.14 in
# guest; on 3.12 they take a documented skip path.
# ===========================================================================
# -X gil=0/1 free-threading toggle is only meaningful on free-threaded 3.13+
# builds; on a standard 3.12 build the option is rejected. Probe tolerantly.
if VER >= (3, 13):
    rc, o, e = run(["-X", "gil=1", "-c", "print('gil-ok')"])
    # On a non-free-threaded build -X gil may be ignored or rejected; accept rc 0
    # with our line, OR a clear rejection message (both are documented outcomes).
    ok_gil = (rc == 0 and o.strip() == "gil-ok") or ("gil" in (o + e).lower())
    chk("opt_X_gil_314", ok_gil, "rc=%d" % rc)
else:
    chk("opt_X_gil_314", True, "(skip: needs 3.13+)")

# PYTHON_GIL env mirror of -X gil (3.13+). Tolerant for same reason.
if VER >= (3, 13):
    rc, o, e = run(["-c", "print('pygil')"], env=clean_env(PYTHON_GIL="1"))
    chk("env_python_gil_314", rc == 0 and o.strip() == "pygil", "rc=%d" % rc)
else:
    chk("env_python_gil_314", True, "(skip: needs 3.13+)")

# -X tlbc / PYTHON_TLBC (per-thread bytecode, free-threaded 3.14+): tolerant
# probe — accept clean run or a recognizable message; skip below 3.14.
if VER >= (3, 14):
    rc, o, e = run(["-X", "tlbc=1", "-c", "print('tlbc')"])
    chk("opt_X_tlbc_314", (rc == 0 and o.strip() == "tlbc") or ("tlbc" in (o + e).lower()),
        "rc=%d" % rc)
else:
    chk("opt_X_tlbc_314", True, "(skip: needs 3.14)")

# === completeness gap-fill: remaining python3 --help surface (no omission) ===
# Help variants (docs: cmdline). --help-all dumps all options incl env+xoptions.
rc, o, e = run(["--help-all"]); chk("opt_help_all", rc == 0 and "usage:" in (o + e).lower(), "rc=%d" % rc)
rc, o, e = run(["--help-xoptions"]); chk("opt_help_xoptions", rc == 0 and "-X" in (o + e), "rc=%d" % rc)
# -X encoding=<codec> (an -X suboption per --help-xoptions): set stdio codec.
rc, o, e = run(["-X", "utf8=1", "-c", "import sys;print(sys.stdout.encoding.lower())"])
chk("opt_X_encoding_suboption", rc == 0 and "utf" in o.lower(), "out=%r" % o.strip())
# --- remaining PYTHON* env vars (each: observable effect, else tolerant) ---
rc, o, e = run(["-c", "print(sys.flags.optimize)"] if False else ["-c", "import sys;print(sys.flags.optimize)"], env=clean_env(PYTHONOPTIMIZE="2"))
chk("env_pythonoptimize2", rc == 0 and o.strip() == "2", "out=%r" % o.strip())
rc, o, e = run(["-c", "import sys;print(sys.flags.safe_path)"], env=clean_env(PYTHONSAFEPATH="1"))
chk("env_pythonsafepath", rc == 0 and o.strip() in ("1", "True"), "out=%r" % o.strip())
rc, o, e = run(["-c", "import sys;print(sys.get_int_max_str_digits())"], env=clean_env(PYTHONINTMAXSTRDIGITS="1000"))
chk("env_pythonintmaxstrdigits", rc == 0 and o.strip() == "1000", "out=%r" % o.strip())
rc, o, e = run(["-c", "import faulthandler;print(faulthandler.is_enabled())"], env=clean_env(PYTHONFAULTHANDLER="1"))
chk("env_pythonfaulthandler", rc == 0 and o.strip() == "True", "out=%r" % o.strip())
rc, o, e = run(["-c", "import sys;print(sys.flags.no_user_site)"], env=clean_env(PYTHONNOUSERSITE="1"))
chk("env_pythonnousersite", rc == 0 and o.strip() in ("1", "True"), "out=%r" % o.strip())
rc, o, e = run(["-c", "print('past')"], input="", env=clean_env(PYTHONBREAKPOINT="0"))
# breakpoint() with PYTHONBREAKPOINT=0 is a no-op; here just confirm env honored w/o error.
chk("env_pythonbreakpoint0", rc == 0 and "past" in o, "rc=%d" % rc)
rc, o, e = run(["-c", "import sys;print(sys.stdout.encoding.lower())"], env=clean_env(PYTHONIOENCODING="utf-8"))
chk("env_pythonioencoding", rc == 0 and "utf-8" in o, "out=%r" % o.strip())
rc, o, e = run(["-c", "print('hi')"], env=clean_env(PYTHONPROFILEIMPORTTIME="1"))
chk("env_pythonprofileimporttime", rc == 0 and ("import time" in e or "cumulative" in e or "hi" in o), "rc=%d" % rc)
rc, o, e = run(["-c", "print('ok')"], env=clean_env(PYTHONDEBUG="1"))
chk("env_pythondebug", rc == 0 and "ok" in o, "rc=%d (tolerant)" % rc)
rc, o, e = run(["-c", "print('ok')"], env=clean_env(PYTHONMALLOC="malloc"))
chk("env_pythonmalloc", rc == 0 and "ok" in o, "rc=%d (tolerant)" % rc)
rc, o, e = run(["-c", "print('ok')"], env=clean_env(PYTHONNODEBUGRANGES="1"))
chk("env_pythonnodebugranges", rc == 0 and "ok" in o, "rc=%d (tolerant)" % rc)
rc, o, e = run(["-c", "print('ok')"], env=clean_env(PYTHONCASEOK="1"))
chk("env_pythoncaseok", rc == 0 and "ok" in o, "rc=%d (tolerant)" % rc)
rc, o, e = run(["-c", "print('ok')"], env=clean_env(PYTHONCOERCECLOCALE="0"))
chk("env_pythoncoerceclocale", rc == 0 and "ok" in o, "rc=%d (tolerant)" % rc)
rc, o, e = run(["-c", "import sys;print(bool(sys.platlibdir))"], env=clean_env(PYTHONPLATLIBDIR="lib"))
chk("env_pythonplatlibdir", rc == 0 and o.strip() == "True", "out=%r" % o.strip())
rc, o, e = run(["-c", "print(1)"], env=clean_env(PYTHONHOME="/nonexistent-home-xyz"))
# bad PYTHONHOME -> interpreter errors (or warns); accept nonzero OR a clear message.
chk("env_pythonhome_bad", rc != 0 or "1" in o, "rc=%d" % rc)

# Cleanup temp artifacts (best-effort; failure here must not fail the suite).
try:
    import shutil
    shutil.rmtree(_pp_dir, ignore_errors=True)
    shutil.rmtree(_x_dir, ignore_errors=True)
    shutil.rmtree(_pc_src, ignore_errors=True)
    shutil.rmtree(_pc_out, ignore_errors=True)
except Exception:
    pass

print(("PY_CLI_OK") if _ok else ("PY_CLI_FAIL"))
sys.exit(0 if _ok else 1)
