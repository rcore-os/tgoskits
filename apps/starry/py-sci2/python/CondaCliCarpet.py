#!/usr/bin/env python3
# CondaCliCarpet.py - exhaustive conda command-line surface carpet.
#
# Ground truth is `conda --help`'s own subcommand tree: every subcommand's `--help` (and `-h`) must
# print its `usage: conda <sub>` banner and exit 0, and the core informational commands
# (--version / info / list / config --show / search --help / run --help ...) must return real,
# well-formed output. Runs the glibc Miniforge `conda` staged at /opt/miniconda on StarryOS.
import os
import subprocess
import sys

ok = 0
fail = 0


def chk(cond, label):
    global ok, fail
    if cond:
        ok += 1
        print("  ok   %s" % label)
    else:
        fail += 1
        print("  FAIL %s" % label)


def _find_conda():
    for c in (os.environ.get("CONDA_EXE"), "/opt/miniconda/bin/conda",
              os.path.expanduser("~/miniconda3/bin/conda")):
        if c and os.path.exists(c):
            return c
    from shutil import which
    return which("conda")


CONDA = _find_conda()
if not CONDA:
    print("CONDACLI_SKIP conda executable not found")
    sys.exit(2)


def run(args, timeout=120):
    env = dict(os.environ)
    env.setdefault("CONDA_ALWAYS_YES", "1")
    r = subprocess.run([CONDA] + args, capture_output=True, text=True, env=env, timeout=timeout)
    return r.returncode, (r.stdout or "") + (r.stderr or "")


# The subcommand tree conda advertises; every one must answer --help with its usage banner.
SUBCOMMANDS = [
    "clean", "compare", "config", "create", "info", "init", "install", "list",
    "notices", "package", "remove", "rename", "run", "search", "update", "env",
    "export", "doctor", "repoquery", "activate", "deactivate",
]

# 1. top-level surface
rc, out = run(["--version"])
chk(rc == 0 and out.strip().lower().startswith("conda "), "conda --version")
chk(any(ch.isdigit() for ch in out), "conda --version has a version number")

rc, out = run(["--help"])
chk(rc == 0 and "usage: conda" in out, "conda --help usage banner")
chk("install" in out and "create" in out and "list" in out, "conda --help lists core subcommands")

rc, out = run(["-h"])
chk(rc == 0 and "usage: conda" in out, "conda -h short form")

# 2. every subcommand's --help + -h
for sub in SUBCOMMANDS:
    rc, out = run([sub, "--help"])
    chk(rc == 0, "conda %s --help exit 0" % sub)
    chk(("usage: conda %s" % sub) in out or ("usage: conda" in out and sub in out),
        "conda %s --help usage banner" % sub)
    chk("-h" in out or "--help" in out, "conda %s --help advertises -h" % sub)
    rc2, out2 = run([sub, "-h"])
    chk(rc2 == 0 and "usage: conda" in out2, "conda %s -h short form" % sub)

# 3. informational commands return real, well-formed output (not just --help)
rc, out = run(["info"])
chk(rc == 0 and "conda version" in out, "conda info reports version")
chk("platform" in out, "conda info reports platform")

rc, out = run(["info", "--json"])
chk(rc == 0 and '"conda_version"' in out, "conda info --json machine-readable")

rc, out = run(["list"])
chk(rc == 0, "conda list exit 0")
chk("numpy" in out or "python" in out, "conda list shows installed packages")

rc, out = run(["list", "numba"])
chk(rc == 0 and "numba" in out, "conda list numba (installed after break)")

rc, out = run(["config", "--show"])
chk(rc == 0 and "channels" in out, "conda config --show has channels")

rc, out = run(["config", "--show-sources"])
chk(rc == 0, "conda config --show-sources exit 0")

rc, out = run(["config", "--describe", "channels"])
chk(rc == 0 and "channels" in out, "conda config --describe channels")

rc, out = run(["env", "list"])
chk(rc == 0 and ("base" in out or "envs" in out or "*" in out), "conda env list shows base")

rc, out = run(["doctor", "--help"])
chk(rc == 0, "conda doctor --help")

rc, out = run(["run", "--help"])
chk(rc == 0 and "usage: conda run" in out, "conda run --help")

rc, out = run(["--version"])
chk(rc == 0 and out.strip().split()[1][0].isdigit(), "conda --version parses to N.N.N")

print("CONDACLI_RESULT ok=%d fail=%d" % (ok, fail))
if fail == 0:
    print("CONDACLI_DONE")
    sys.exit(0)
sys.exit(1)
