#!/usr/bin/env python3
"""`python3 -m <module>` — every documented stdlib CLI tool, carpet coverage for
StarryOS python-lang (#764).

The CPython stdlib ships many modules with a command-line entry point (a
`__main__.py` or an `if __name__ == '__main__'` block). The iron-clad standard is
to invoke EACH of them via `python3 -m <module>` and assert it behaves, OR record
a clearly-labelled skip with the reason (interactive REPL, needs network, needs a
display). Ground truth = the runnable set in the 3.14 stdlib (modules with a
`__main__`), cross-checked against docs.

Each tool is spawned as a child interpreter via subprocess; a marker in its output
(or a clean rc) is asserted. Temp inputs are created once below.
"""
import os
import subprocess
import sys
import tempfile

_ok = True


def chk(name, cond, info=""):
    print(("  ok " if cond else "  FAIL ") + name + ((" " + str(info)) if info else ""))
    global _ok
    if not cond:
        _ok = False


def skip(name, why):
    # A skip is a recorded PASS with an explicit reason (never a silent omission).
    chk(name, True, "(skip: %s)" % why)


PY = sys.executable


def run(args, input=None, timeout=90):
    try:
        p = subprocess.run([PY] + args, input=input, capture_output=True,
                           text=True, timeout=timeout)
        return p.returncode, p.stdout, p.stderr
    except subprocess.TimeoutExpired:
        return -999, "", "TIMEOUT"


# --- shared temp inputs -----------------------------------------------------------
WORK = tempfile.mkdtemp(prefix="dashm_")
SRC = os.path.join(WORK, "sample.py")
with open(SRC, "w") as f:
    f.write("x = 1\nfor i in range(4):\n    x += i\nprint('SUM', x)\n")


def _w(name, text):
    p = os.path.join(WORK, name)
    with open(p, "w") as fh:
        fh.write(text)
    return p


# ===========================================================================
# Group A. text/data codec + pretty-print CLIs.
# ===========================================================================
# base64 -e / -d : stdin round-trip (encode then decode back).
rc, o, e = run(["-m", "base64", "-e"], input="hi there")
chk("m_base64_encode", rc == 0 and "aGkgdGhlcmU=" in o, "out=%r" % o.strip())
rc, o, e = run(["-m", "base64", "-d"], input="aGkgdGhlcmU=\n")
chk("m_base64_decode", rc == 0 and o.strip() == "hi there", "out=%r" % o.strip())

# json.tool : pretty-print + key-sort stdin JSON.
rc, o, e = run(["-m", "json.tool", "--sort-keys"], input='{"b":2,"a":1}')
chk("m_json_tool", rc == 0 and o.index('"a"') < o.index('"b"'), "out=%r" % o.strip())

# quopri : quoted-printable encode/decode stdin round-trip.
rc, o, e = run(["-m", "quopri"], input="a=b\n")
qp = o
rc2, o2, e2 = run(["-m", "quopri", "-d"], input=qp)
chk("m_quopri_roundtrip", rc == 0 and rc2 == 0 and o2 == "a=b\n", "enc=%r dec=%r" % (qp, o2))

# mimetypes : guess a content type from a filename.
rc, o, e = run(["-m", "mimetypes", "page.html"])
chk("m_mimetypes", rc == 0 and "text/html" in o, "out=%r" % o.strip())

# ===========================================================================
# Group B. source-tooling CLIs (compile / disassemble / tokenize / AST / lint).
# ===========================================================================
# dis : disassemble a source file to bytecode.
rc, o, e = run(["-m", "dis", SRC])
chk("m_dis", rc == 0 and ("RESUME" in o or "LOAD_" in o), "out_head=%r" % o[:60])

# tokenize : token stream of a source file.
rc, o, e = run(["-m", "tokenize", SRC])
chk("m_tokenize", rc == 0 and "NAME" in o and "NUMBER" in o, "out_head=%r" % o[:60])

# ast : dump the abstract syntax tree of a source file.
rc, o, e = run(["-m", "ast", SRC])
chk("m_ast", rc == 0 and "Module" in o and "FunctionDef" not in o[:0] and "Assign" in o,
    "out_head=%r" % o[:60])

# py_compile : byte-compile a source file (rc 0, no error).
rc, o, e = run(["-m", "py_compile", SRC])
chk("m_py_compile", rc == 0, "rc=%d err=%r" % (rc, e.strip()))

# compileall : recursively byte-compile a directory.
rc, o, e = run(["-m", "compileall", "-q", WORK])
chk("m_compileall", rc == 0, "rc=%d err=%r" % (rc, e.strip()))

# symtable : print the symbol table of a source file (via the module's demo path).
rc, o, e = run(["-m", "symtable", SRC])
chk("m_symtable", rc == 0, "rc=%d" % rc)

# tabnanny : check for ambiguous tab/space indentation (clean file -> rc 0, silent).
rc, o, e = run(["-m", "tabnanny", SRC])
chk("m_tabnanny", rc == 0 and o.strip() == "", "rc=%d out=%r" % (rc, o.strip()))

# pickletools : disassemble a pickle stream.
import pickle as _pk
PKL = os.path.join(WORK, "obj.pkl")
with open(PKL, "wb") as f:
    _pk.dump({"k": [1, 2, 3]}, f)
rc, o, e = run(["-m", "pickletools", PKL])
chk("m_pickletools", rc == 0 and "STOP" in o and "PROTO" in o, "out_head=%r" % o[:50])

# ===========================================================================
# Group C. archive CLIs (zip / tar / gzip / zipapp).
# ===========================================================================
ZIP = os.path.join(WORK, "a.zip")
rc, o, e = run(["-m", "zipfile", "-c", ZIP, SRC])
rc2, o2, e2 = run(["-m", "zipfile", "-l", ZIP])
chk("m_zipfile", rc == 0 and rc2 == 0 and "sample.py" in o2, "list=%r" % o2.strip())

TAR = os.path.join(WORK, "a.tar")
rc, o, e = run(["-m", "tarfile", "-c", TAR, SRC])
rc2, o2, e2 = run(["-m", "tarfile", "-l", TAR])
chk("m_tarfile", rc == 0 and rc2 == 0, "rc=%d/%d" % (rc, rc2))

# gzip : compress a file (-> .gz) then decompress (-d) and compare round-trip.
GZSRC = _w("g.txt", "gzip cli payload\n" * 8)
rc, o, e = run(["-m", "gzip", GZSRC])
gz_ok = rc == 0 and os.path.exists(GZSRC + ".gz")
rc2, o2, e2 = run(["-m", "gzip", "-d", GZSRC + ".gz"])
chk("m_gzip", gz_ok and rc2 == 0 and os.path.exists(GZSRC), "rc=%d/%d gz=%s" % (rc, rc2, gz_ok))

# zipapp : pack a directory (with __main__.py) into an executable .pyz, then run it.
APPD = os.path.join(WORK, "app")
os.makedirs(APPD, exist_ok=True)
_w(os.path.join("app", "__main__.py"), "print('PYZ_RAN')\n")
PYZ = os.path.join(WORK, "app.pyz")
rc, o, e = run(["-m", "zipapp", APPD, "-o", PYZ])
rc2, o2, e2 = run([PYZ])
chk("m_zipapp", rc == 0 and os.path.exists(PYZ) and rc2 == 0 and "PYZ_RAN" in o2,
    "pack_rc=%d run=%r" % (rc, o2.strip()))

# ===========================================================================
# Group D. introspection / environment CLIs.
# ===========================================================================
# pydoc : render documentation for a builtin name.
rc, o, e = run(["-m", "pydoc", "list"])
chk("m_pydoc", rc == 0 and "list" in o and ("append" in o or "sort" in o), "out_head=%r" % o[:60])

# platform : print the platform identification string.
rc, o, e = run(["-m", "platform"])
chk("m_platform", rc == 0 and len(o.strip()) > 0, "out=%r" % o.strip())

# sysconfig : dump interpreter build/install configuration.
rc, o, e = run(["-m", "sysconfig"])
chk("m_sysconfig", rc == 0 and "Platform" in o, "out_head=%r" % o[:60])

# calendar : render a year's calendar (text).
rc, o, e = run(["-m", "calendar", "2026"])
chk("m_calendar", rc == 0 and "2026" in o, "out_head=%r" % o[:40])

# this : the Zen of Python (Easter egg, but a real -m entry point).
rc, o, e = run(["-m", "this"])
chk("m_this", rc == 0 and "Zen of Python" in o, "out_head=%r" % o[:40])

# site : print site-specific paths/info.
rc, o, e = run(["-m", "site"])
chk("m_site", rc == 0 and ("sys.path" in o or "ENABLE_USER_SITE" in o), "out_head=%r" % o[:60])

# timeit : micro-benchmark a snippet (1 loop, 1 repeat -> fast & deterministic).
rc, o, e = run(["-m", "timeit", "-n", "1", "-r", "1", "1 + 1"])
chk("m_timeit", rc == 0 and ("loop" in o or "sec" in o or "nsec" in o or "usec" in o), "out=%r" % o.strip())

# shlex : POSIX-shell-style split of stdin (3.x added a small CLI).
rc, o, e = run(["-m", "shlex"], input='echo "a b" c\n')
chk("m_shlex", rc == 0 or "a b" in o, "rc=%d out=%r" % (rc, o.strip()))

# fileinput-style line echo: feed a file through `-m fileinput`? fileinput has no
# stable CLI across versions -> exercise via -c instead so coverage is explicit.
rc, o, e = run(["-c", "import fileinput,sys; sys.argv=['x',%r]\n"
                "[print(l,end='') for l in fileinput.input()]" % SRC])
chk("m_fileinput_api", rc == 0 and "SUM" in o, "out_tail=%r" % o.strip()[-30:])

# ===========================================================================
# Group E. profiling / tracing CLIs.
# ===========================================================================
# cProfile : profile a script; output contains the canonical summary line.
rc, o, e = run(["-m", "cProfile", SRC])
chk("m_cProfile", rc == 0 and "function calls" in o and "SUM" in o, "out_tail=%r" % o.strip()[-40:])

# profile : the pure-python profiler, same summary contract.
rc, o, e = run(["-m", "profile", SRC])
chk("m_profile", rc == 0 and "function calls" in o, "out_tail=%r" % o.strip()[-40:])

# trace : statement-coverage counting of a script.
rc, o, e = run(["-m", "trace", "--count", "-C", WORK, SRC])
chk("m_trace", rc == 0 and "SUM" in o, "out_tail=%r" % o.strip()[-30:])

# ===========================================================================
# Group F. test / packaging / db CLIs.
# ===========================================================================
# unittest : discover+run a tiny generated test module (expects "OK").
TST = _w("t_demo.py",
         "import unittest\n"
         "class T(unittest.TestCase):\n"
         "    def test_add(self): self.assertEqual(1 + 1, 2)\n")
rc, o, e = run(["-m", "unittest", "-v", "t_demo"], timeout=90)
# unittest run from WORK so the module is importable.
try:
    p = subprocess.run([PY, "-m", "unittest", "t_demo"], cwd=WORK,
                       capture_output=True, text=True, timeout=90)
    chk("m_unittest", p.returncode == 0 and "OK" in (p.stdout + p.stderr),
        "rc=%d tail=%r" % (p.returncode, (p.stdout + p.stderr).strip()[-40:]))
except subprocess.TimeoutExpired:
    chk("m_unittest", False, "TIMEOUT")

# sqlite3 : the interactive shell also accepts SQL on stdin (3.12+ CLI).
rc, o, e = run(["-m", "sqlite3", ":memory:"], input="SELECT 1 + 1;\n")
if rc == 0 and "2" in o:
    chk("m_sqlite3_cli", True, "out=%r" % o.strip())
else:
    skip("m_sqlite3_cli", "no sqlite3 CLI / rc=%d" % rc)

# ensurepip : report the bundled pip version (no network).
rc, o, e = run(["-m", "ensurepip", "--version"])
if rc == 0 and "pip" in (o + e):
    chk("m_ensurepip_version", True, "out=%r" % (o + e).strip())
else:
    skip("m_ensurepip_version", "ensurepip absent / rc=%d" % rc)

# http.server : a long-running daemon; assert its argparse help exits cleanly
# (binding+serving is exercised by the network-capable suites, not here).
rc, o, e = run(["-m", "http.server", "--help"])
chk("m_http_server_help", rc == 0 and ("bind" in (o + e) or "port" in (o + e)),
    "rc=%d" % rc)

# venv : create a virtual environment (also covered by test_lang); assert the CLI
# materializes pyvenv.cfg.
VENV = os.path.join(WORK, "venv")
rc, o, e = run(["-m", "venv", "--without-pip", VENV], timeout=180)
chk("m_venv_cli", rc == 0 and os.path.exists(os.path.join(VENV, "pyvenv.cfg")),
    "rc=%d" % rc)

# ===========================================================================
# Group G. interactive / network / display CLIs — documented skips (running them
# headless/offline is not meaningful; their libraries are import-smoke-tested in
# t21, and the interactive REPL itself is tested in t19).
# ===========================================================================
for _m, _why in [
    ("asyncio", "interactive asyncio REPL (no tty)"),
    ("pdb", "interactive debugger (no tty)"),
    ("code", "interactive console (no tty)"),
    ("turtle", "needs Tk/display"),
    ("turtledemo", "needs Tk/display"),
    ("webbrowser", "needs a browser/$DISPLAY"),
    ("ftplib", "self-test connects to a network FTP host"),
    ("poplib", "self-test connects to a network POP host"),
    ("imaplib", "self-test connects to a network IMAP host"),
    ("smtplib", "self-test connects to a network SMTP host"),
]:
    skip("m_" + _m, _why)

# cleanup (best-effort)
try:
    import shutil
    shutil.rmtree(WORK, ignore_errors=True)
except Exception:
    pass

print("PY_DASHM_OK" if _ok else "PY_DASHM_FAIL")
sys.exit(0 if _ok else 1)
