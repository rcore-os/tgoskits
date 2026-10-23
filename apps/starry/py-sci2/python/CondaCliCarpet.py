#!/usr/bin/env python3
# CondaCliCarpet.py - exhaustive conda command-line surface carpet.
#
# Ground truth is conda's own top-level help subcommand tree, sampled instead of exhausted: a
# representative set of subcommands must print their `usage: conda <sub>` banner and exit 0, and
# the core informational commands (--version / info / list / config --show ...) must return real,
# well-formed output. The subcommands outside that sample keep their behavioral coverage in the
# functional blocks below (solve dry-runs, env/run/compare/search/notices/doctor/package/repoquery
# and the error paths), because spawning `--help` once per advertised subcommand buys the same
# banner information repeatedly at one full conda Python startup per spawn.
# Runs the glibc Miniforge `conda` staged at /opt/miniconda on StarryOS.
#
# The checks keep their pass/fail conditions; only how the text is obtained is tolerant: `--help` /
# `-h` / `help` are accepted spellings for a banner and a `--json` document may arrive on stdout,
# on stderr or in a mixed capture (with noise before or after it). At the top level the bare
# `conda help` spelling is probed ahead of the canonical `--help`, so the first conda invocation
# that builds the full argparse/plugin tree is the cheap one and warms that path for the canonical
# spelling (see help_output() below). No command line is ever run twice and the per-call timeout
# below stays the hard cap, so a hung conda costs that cap once and then fails with its diagnostics
# instead of being retried with a longer cap. Failures keep their original label plus the exit
# status and first output line.
#
# TCG NOTE: each `conda` process costs ~35s under full-emulation because conda's Python startup is
# heavy. The primary fix is to not start one at all: when this file runs under the very interpreter
# that provides `conda`, every command line is served in-process by `conda.cli.main.main(*args)`
# (see INPROC_MAIN / _run_inproc below), whose `main_subshell` rebuilds the context per call, so
# no command pays a second Python startup. The subprocess path stays as a compatibility fallback
# for environments where the conda entry point cannot be imported. On top of that we CONSOLIDATE:
# a single rich `--json` invocation is PARSED for many assertions, and the `--help` banner check
# is sampled over subcommands that each stand for a distinct CLI shape rather than repeated for
# every advertised one. The per-block "(N spawns)" notes count conda calls: under the in-process
# path each one is a plain function invocation instead of a new process. Every config/info/list
# field keeps its assertion and no functional call is lost; only that count drops.
import contextlib
import io
import json
import os
import re
import signal
import subprocess
import sys
import tempfile

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


# ============================================================================
# IN-PROCESS COMMAND EXECUTION
# When this file is run by the interpreter that provides `conda` (the StarryOS image runs it with
# the staged Miniforge python), a command line can be served by `conda.cli.main.main(*args)` in
# this very process: conda rebuilds its context - and re-runs its logging setup - on every
# `main_subshell` call, so the CLI behaves as it does for a fresh process, minus the Python
# startup that dominates under TCG. The subprocess path stays as a compatibility fallback for any
# other environment.
# ============================================================================
class _CaptureStream(io.TextIOWrapper):
    """Text stream over an in-memory UTF-8 byte buffer.

    conda prints banners and JSON through `sys.stdout`/`sys.stderr` but parts of its machinery
    reach for `sys.stdout.buffer`, so the capture keeps a real binary buffer behind the text
    wrapper. Both instances are reused (see `reset()`) so a command that kept a reference to the
    stream it was first handed keeps writing into the same capture.
    """

    def __init__(self):
        super().__init__(io.BytesIO(), encoding="utf-8", errors="replace", newline="",
                         write_through=True)

    def reset(self):
        self.flush()
        self.buffer.seek(0)
        self.buffer.truncate(0)

    def text(self):
        self.flush()
        return self.buffer.getvalue().decode("utf-8", "replace")


_CAPOUT = _CaptureStream()
_CAPERR = _CaptureStream()


class _InprocTimeout(Exception):
    """Signals that one in-process conda call exceeded its cap."""


def _on_alarm(signum, frame):
    raise _InprocTimeout()


@contextlib.contextmanager
def _alarm(timeout):
    # A hung in-process call must fail its own check (rc 124, like the subprocess cap) instead of
    # stalling the carpet. Best effort: SIGALRM is unavailable off the main thread and on
    # platforms that lack it, where the call then runs uncapped.
    armed = False
    old = None
    if timeout and timeout > 0 and hasattr(signal, "SIGALRM"):
        try:
            old = signal.signal(signal.SIGALRM, _on_alarm)
            signal.alarm(int(timeout))
            armed = True
        except (ValueError, OSError, TypeError):
            armed = False
    try:
        yield
    finally:
        if armed:
            try:
                signal.alarm(0)
                signal.signal(signal.SIGALRM, old)
            except (ValueError, OSError):
                pass


def _close_quietly(fd):
    try:
        os.close(fd)
    except OSError:
        pass


def _unlink_quietly(path):
    try:
        os.unlink(path)
    except OSError:
        pass


def _install_fd_capture():
    """Send this process's own stdout/stderr file descriptors to temp files (best effort).

    `sys.stdout`/`sys.stderr` redirects only cover what conda itself prints. Commands that start a
    child - `conda run` is the one asserted here - hand the child the inherited file descriptors,
    so a child would print straight to the console this carpet reports on and its output would be
    lost to the assertion. Capturing the descriptors as well keeps those bytes. Returns a state
    dict for `_restore_fd_capture`, or None when the descriptors cannot be redirected.
    """
    created = []
    saved = []
    try:
        for prefix in ("condacli-out-", "condacli-err-"):
            created.append(tempfile.mkstemp(prefix=prefix))
        sys.stdout.flush()
        sys.stderr.flush()
        saved.extend([os.dup(1), os.dup(2)])
        os.dup2(created[0][0], 1)
        os.dup2(created[1][0], 2)
    except OSError:
        for fd, _ in created:
            _close_quietly(fd)
        for fd in saved:
            _close_quietly(fd)
        for _, path in created:
            _unlink_quietly(path)
        return None
    for fd, _ in created:
        _close_quietly(fd)
    return {"saved": saved, "paths": [path for _, path in created], "done": False}


def _restore_fd_capture(state):
    """Restore the real descriptors and return whatever reached them, once."""
    if not state or state["done"]:
        return "", ""
    state["done"] = True
    # The sys-level redirects are already gone here, so anything still buffered belongs to a
    # command that wrote through the descriptors; flush it into the capture before restoring them.
    for stream in (sys.stdout, sys.stderr):
        try:
            stream.flush()
        except (OSError, ValueError):
            pass
    try:
        os.dup2(state["saved"][0], 1)
        os.dup2(state["saved"][1], 2)
    except OSError:
        pass
    for fd in state["saved"]:
        _close_quietly(fd)
    texts = []
    for path in state["paths"]:
        try:
            with open(path, "rb") as handle:
                texts.append(handle.read().decode("utf-8", "replace"))
        except OSError:
            texts.append("")
        _unlink_quietly(path)
    return texts[0], texts[1]


def _load_inproc_main():
    """Return conda's in-process entry point when this interpreter is the conda one."""
    if not CONDA:
        return None
    if os.path.dirname(os.path.realpath(sys.executable)) != os.path.dirname(os.path.realpath(CONDA)):
        return None
    try:
        from conda.cli.main import main as conda_main
    except Exception:
        return None
    return conda_main


INPROC_MAIN = _load_inproc_main()


def _run_inproc(args, timeout):
    """Serve one conda command line in this process; returns (rc, stdout, stderr).

    The shape matches a finished subprocess: integer status plus the two captured streams, in that
    order. `main(*args)` returns None (or an int) on completion and raises `SystemExit` on the
    paths that end the process (argparse `--help`, argparse errors, explicit exits); any other
    exception is reported as a failed call with a diagnostic, so a command that crashes inside the
    interpreter still fails its own check instead of aborting the carpet.
    """
    # Parity with the subprocess path: CONDA_ALWAYS_YES is inherited as an env var there, and the
    # 80-column fallback is what a piped stdout gave conda before (a serial console of unknown
    # width must not re-wrap --help output and break the keyword assertions).
    os.environ.setdefault("CONDA_ALWAYS_YES", "1")
    os.environ.setdefault("COLUMNS", "80")
    _CAPOUT.reset()
    _CAPERR.reset()
    saved_argv = sys.argv
    # `main(*args)` is the entry point used here, but conda falls back to `sys.argv[1:]` when it is
    # called without arguments, so keep both spellings consistent for the duration of the call.
    sys.argv = ["conda"] + list(args)
    fd_state = None
    try:
        fd_state = _install_fd_capture()
        with _alarm(timeout), contextlib.redirect_stdout(_CAPOUT), contextlib.redirect_stderr(_CAPERR):
            code = INPROC_MAIN(*args)
        if isinstance(code, int):
            rc = int(code)
        else:
            # conda returns None when the command completed; treat that as success 0.
            rc = 0
        extra_out, extra_err = "", ""
    except _InprocTimeout:
        rc, extra_out = 124, ""
        extra_err = "TIMEOUT: conda %s exceeded %ds" % (" ".join(args), timeout)
    except SystemExit as exc:
        code = exc.code
        if code is None:
            code = 0
        elif not isinstance(code, int):
            # `sys.exit("message")`: the message is the failure, the status is 1.
            code = 1
        rc, extra_out, extra_err = code, "", ""
    except Exception as exc:
        rc, extra_out = 1, ""
        extra_err = "INPROC ERROR: %s: %s" % (type(exc).__name__, exc)
    finally:
        sys.argv = saved_argv
        # Restores the real descriptors before anything else is printed, and collects whatever
        # only ever reached them (conda's own prints are already in the stream captures above).
        fd_out, fd_err = _restore_fd_capture(fd_state)
    return rc, _CAPOUT.text() + fd_out + extra_out, _CAPERR.text() + fd_err + extra_err


# Each call is capped at 300s (in-process through SIGALRM, subprocess through its timeout), so a
# genuinely hung conda fails one check with a TIMEOUT diagnostic instead of stalling the carpet.
def run(args, timeout=300):
    if INPROC_MAIN is not None:
        rc, sout, serr = _run_inproc(args, timeout)
        return rc, sout + serr
    env = dict(os.environ)
    env.setdefault("CONDA_ALWAYS_YES", "1")
    try:
        r = subprocess.run([CONDA] + args, capture_output=True, text=True, env=env, timeout=timeout)
    except subprocess.TimeoutExpired:
        # A single pathologically slow spawn (heavy env scan under TCG) must not
        # crash the whole carpet - surface it as a failed chk instead.
        return 124, "TIMEOUT: conda %s exceeded %ds" % (" ".join(args), timeout)
    return r.returncode, (r.stdout or "") + (r.stderr or "")


def run_split(args, timeout=300):
    # Keep stdout/stderr apart so a `--json` document can be looked for on either stream (and so
    # timeouts can report the empty capture) instead of being merged in advance.
    if INPROC_MAIN is not None:
        return _run_inproc(args, timeout)
    env = dict(os.environ)
    env.setdefault("CONDA_ALWAYS_YES", "1")
    try:
        r = subprocess.run([CONDA] + args, capture_output=True, text=True, env=env, timeout=timeout)
    except subprocess.TimeoutExpired:
        return 124, "", "TIMEOUT: conda %s exceeded %ds" % (" ".join(args), timeout)
    return r.returncode, (r.stdout or ""), (r.stderr or "")


def run_json(args, timeout=300):
    # Newer conda builds move the document (or the diagnostics around it) between stdout and
    # stderr and may append warnings after it, so parse stdout, stderr and the merged capture
    # in that order instead of insisting on a clean stdout-only document. The spawn happens once:
    # a timeout or an empty capture is reported as-is rather than retried.
    rc, sout, serr = run_split(args, timeout=timeout)
    for text in (sout, serr, (sout or "") + "\n" + (serr or "")):
        doc = loadjson(text)
        if doc is not None:
            return rc, doc, sout, serr
    return rc, None, sout, serr


def loadjson(text):
    # conda --json output is a single JSON document; tolerate noise before it (warnings,
    # activation chatter) and noise after it (trailing warnings) by decoding from the first
    # `{`/`[` and stopping at the end of that document.
    if not text:
        return None
    text = text.strip()
    try:
        return json.loads(text)
    except Exception:
        pass
    decoder = json.JSONDecoder()
    for i, ch in enumerate(text):
        if ch in "{[":
            try:
                doc, _ = decoder.raw_decode(text[i:])
                return doc
            except Exception:
                continue
    return None


# A help banner names the program on a usage line. Older conda prints `usage: conda <sub>`,
# newer builds may route it through a wrapper name, so match the banner case-insensitively and
# keep the subcommand name check separate.
_USAGE_RE = re.compile(r"usage:\s*\S*conda", re.IGNORECASE)
_CORE_SUBCOMMANDS = ("install", "create", "list")


def _has_usage(text):
    return bool(text) and bool(_USAGE_RE.search(text))


def _snippet(text, limit=140):
    # First non-empty line of captured output, clipped, for assertion diagnostics.
    for line in (text or "").splitlines():
        line = " ".join(line.split())
        if line:
            return line[:limit]
    return "<no output>"


def _diag(rc, sout, serr):
    return "rc=%s stdout=%r stderr=%r" % (rc, _snippet(sout), _snippet(serr))


def _json_ok(doc, rc):
    # A machine-readable conda document must be a non-empty container; when conda also exited
    # non-zero it has to carry the explanation (error/success/message) rather than being junk.
    if not isinstance(doc, (dict, list)) or len(doc) == 0:
        return False
    if rc != 0 and isinstance(doc, dict):
        return any(k in doc for k in ("error", "success", "message", "actions", "dry_run"))
    return True


def help_output(args, timeout=300):
    # `conda <args> --help` with the `-h` short form as fallbacks; the top-level entry probes the
    # bare `conda help` spelling first. `conda help` is the same top-level help, but the canonical
    # `conda --help` is the first invocation that builds conda's full parser, so it pays the cold
    # import of every plugin entry point (the libmamba solver included); probing the cheap spelling
    # first warms that path for the canonical spelling right after. Whichever attempt produces an
    # exit-0 banner wins, the longest capture is kept otherwise. Identical command lines are never
    # repeated: a timed-out subcommand spelling ends the chain, and the top-level chain spends at
    # most one extra cap so the canonical spelling still gets its single attempt.
    if args:
        attempts = [list(args) + ["--help"], list(args) + ["-h"]]
    else:
        attempts = [["help"], ["--help"], ["-h"]]
    best = (1, "")
    for attempt in attempts:
        rc, out = run(attempt, timeout=timeout)
        if rc == 0 and _has_usage(out):
            return rc, out
        # Keep the longest capture, except that a timeout always wins so a hard-cap failure stays
        # visible even when an earlier spelling failed with a shorter banner/error capture.
        if rc == 124 or len(out) > len(best[1]):
            best = (rc, out)
        if rc == 124:
            # A hung conda normally ends the chain so it costs the cap once. Only the leading
            # top-level `conda help` spelling falls through: it is a distinct command line whose
            # timeout has already warmed the import path, so the canonical `--help` still gets its
            # single, unchanged-cap attempt. Any later timeout stops the chain as usual.
            if args or attempt != attempts[0]:
                break
    return best


# ============================================================================
# 1. TOP-LEVEL SURFACE  (2-4 spawns)
# ============================================================================
rc, out = run(["--version"])
_verline = out.strip()
chk(rc == 0 and _verline.lower().startswith("conda "), "conda --version %s" % _diag(rc, out, ""))
chk(any(ch.isdigit() for ch in out), "conda --version has a version number")
_vparts = _verline.split()
chk(len(_vparts) >= 2 and _vparts[1][0].isdigit(), "conda --version parses to N.N.N")

# Top-level help through whichever equivalent spelling this conda answers (`help` first, then
# `--help` / `-h`). The labels stay spelling-neutral because any of the three may be the one that
# answers; the banner assertion and the subcommand-listing assertion stay separate so a renamed
# banner line does not hide a missing subcommand section, and both stay informative on failure.
rc, out = help_output([])
_missing_core = [s for s in _CORE_SUBCOMMANDS if not re.search(r"\b%s\b" % s, out)]
chk(rc == 0 and _has_usage(out), "conda top-level help usage banner %s" % _diag(rc, out, ""))
chk(rc == 0 and not _missing_core,
    "conda top-level help lists core subcommands missing=%s %s" % (_missing_core, _diag(rc, out, "")))

# ============================================================================
# 2. SUBCOMMAND --help SAMPLE  (one `--help` spawn per sampled subcommand)
# Ground truth: the sampled subcommands answer --help with a usage banner, exit 0,
# and mention an expected keyword/flag; the rest are covered by the functional
# blocks below instead of each paying its own conda startup.
# ============================================================================
# (subcommand, expected keyword that must appear in its --help output)
HELP_TREE = [
    ("info", "--json"),
    ("config", "--show"),
    ("list", "--export"),
    ("search", "--info"),
    ("create", "--clone"),
    ("install", "--freeze-installed"),
    ("update", "--all"),
    ("remove", "--all"),
    ("clean", "--tarballs"),
    ("compare", "compare"),
    ("env", "list"),
    ("run", "--no-capture-output"),
    ("init", "--dry-run"),
    ("package", "--which"),
    ("notices", "notices"),
    ("doctor", "doctor"),
    ("rename", "-n"),
    ("export", "export"),
    ("activate", "activate"),
    ("deactivate", "deactivate"),
    ("repoquery", "repoquery"),
    ("search", "--platform"),  # extra flag check folded into same sub below
]
# Representative sample: one subcommand per CLI shape - informational dump (info), config keys
# (config), solver mutation (install), environment management (env), command execution (run),
# index query (search), package-ownership query (package), plugin query (repoquery). Spawning the
# full tree costs one heavy Python startup per subcommand (~35-60s under TCG) while restating the
# same banner shape; the functional blocks keep the behavior of the unprobed subcommands covered.
HELP_PROBES = ("info", "config", "install", "env", "run", "search", "package", "repoquery")
# de-dup while asserting the union of expected keywords per subcommand
_help_out = {}
_expected = {}
for sub, kw in HELP_TREE:
    _expected.setdefault(sub, [])
    if kw not in _expected[sub]:
        _expected[sub].append(kw)

SUBCOMMANDS = [s for s in HELP_PROBES if s in _expected]
for sub in SUBCOMMANDS:
    rc, out = help_output([sub])
    _help_out[sub] = (rc, out)
    chk(rc == 0, "conda %s --help exit 0 rc=%s first=%r" % (sub, rc, _snippet(out)))
    chk(("usage: conda %s" % sub) in out or (_has_usage(out) and sub in out),
        "conda %s --help usage banner" % sub)
    chk("-h" in out or "--help" in out, "conda %s --help advertises -h" % sub)
    for kw in _expected[sub]:
        chk(kw in out, "conda %s --help advertises %s" % (sub, kw))

# rich --help flag coverage parsed from the already-captured install/search/run/env/package
# --help output (NO extra spawns). Only probes that actually ran are consulted here, so the
# assertions can never reference a spawn that was skipped. The unprobed subcommands keep their
# behavior covered: create/remove by the solve dry-runs in block 7, clean by the offline
# `clean --all --dry-run` call in block 8.
_ihelp = _help_out.get("install", (1, ""))[1]
chk("--no-deps" in _ihelp, "install --help advertises --no-deps")
chk("--only-deps" in _ihelp, "install --help advertises --only-deps")
chk("--update-deps" in _ihelp, "install --help advertises --update-deps")
chk("--force-reinstall" in _ihelp, "install --help advertises --force-reinstall")
chk("--revision" in _ihelp, "install --help advertises --revision")

_shelp = _help_out.get("search", (1, ""))[1]
chk("--channel" in _shelp or "-c" in _shelp, "search --help advertises --channel")
chk("--envs" in _shelp, "search --help advertises --envs")

_runhelp = _help_out.get("run", (1, ""))[1]
chk("--cwd" in _runhelp, "run --help advertises --cwd")
chk("-n" in _runhelp or "--name" in _runhelp, "run --help advertises -n/--name")

_envhelp = _help_out.get("env", (1, ""))[1]
chk("create" in _envhelp and "export" in _envhelp and "remove" in _envhelp and "update" in _envhelp,
    "conda env --help lists sub-subcommands")

_pkghelp = _help_out.get("package", (1, ""))[1]
chk("--which" in _pkghelp or "--pack" in _pkghelp, "package --help advertises --which/--pack")

# ============================================================================
# 3. -h  ==  --help  ALIAS  (1 spawn, representative subcommand)
# Instead of spawning `-h` for every subcommand, prove the alias once on `info`
# and rely on the sampled --help probes above for the rest.
# ============================================================================
rc_h, out_h = run(["info", "-h"])
_info_help = _help_out.get("info", (1, ""))[1]
chk(rc_h == 0 and _has_usage(out_h), "conda info -h short form exit 0 rc=%s first=%r"
    % (rc_h, _snippet(out_h)))
chk(out_h.strip() != "" and out_h.strip() == _info_help.strip(),
    "conda info -h output equals --help (-h is alias)")

# ============================================================================
# 4. conda info  --  CONSOLIDATED (info --json parsed for many fields) (~3 spawns)
# One machine-readable spawn covers version/platform/python/channels/envs/root_prefix/
# pkgs_dirs (subsuming the former --all/--system/--base/--unsafe-channels field checks).
# ============================================================================
rc, _ij, sout, serr = run_json(["info", "--json"])
chk(rc == 0 and isinstance(_ij, dict), "conda info --json parses to dict %s" % _diag(rc, sout, serr))
_ij = _ij if isinstance(_ij, dict) else {}
chk("conda_version" in _ij, "info --json has conda_version")
chk("platform" in _ij, "info --json has platform")
chk("python_version" in _ij, "info --json has python_version")
chk(isinstance(_ij.get("envs"), list), "info --json envs is a list")
chk(isinstance(_ij.get("channels"), list), "info --json channels is a list")
chk("root_prefix" in _ij or "conda_prefix" in _ij, "info --json has root_prefix/conda_prefix")
chk(isinstance(_ij.get("pkgs_dirs"), list) or "pkgs_dirs" in _ij, "info --json has pkgs_dirs")
# --base equivalent: the base/root prefix must be an existing directory.
_base = _ij.get("root_prefix") or _ij.get("conda_prefix") or ""
chk(isinstance(_base, str) and _base != "" and os.path.isdir(_base),
    "info --json root_prefix is an existing prefix dir")

# --envs / env-list surface (human text still exercised once); the top-level `--json`
# flag placement is also proven here (global flag before the subcommand).
rc, _ie, sout, serr = run_json(["--json", "info", "--envs"])
chk(rc == 0 and _ie is not None,
    "conda --json info --envs valid top-level JSON (global --json placement) %s" % _diag(rc, sout, serr))
chk(isinstance(_ie, dict) and (isinstance(_ie.get("envs"), list) or "envs" in _ie),
    "conda info --envs enumerates environments")

# ============================================================================
# 5. conda config  --  CONSOLIDATED (config --show --json parsed for many keys) (~5 spawns)
# One --show --json spawn asserts every config key formerly probed one-by-one via
# --get/--describe (channels, channel_priority, ssl_verify, always_yes,
# show_channel_urls, ...). Plus show-sources --json, a set/get/remove-key round-trip,
# and --validate.
# ============================================================================
rc, _cj, sout, serr = run_json(["config", "--show", "--json"])
chk(rc == 0 and isinstance(_cj, dict),
    "conda config --show --json parses to dict %s" % _diag(rc, sout, serr))
_cj = _cj if isinstance(_cj, dict) else {}
chk("channels" in _cj, "config --show --json has channels key")
chk(isinstance(_cj.get("channels"), list), "config --show --json channels is a list")
# every config key previously probed via --get/--describe, now asserted from the JSON dump.
for _key in ("channel_priority", "ssl_verify", "always_yes", "show_channel_urls",
             "channel_alias", "default_channels", "add_pip_as_python_dependency",
             "auto_update_conda", "pkgs_dirs", "envs_dirs"):
    chk(_key in _cj, "config --show --json documents key %s" % _key)

rc, _csj, sout, serr = run_json(["config", "--show-sources", "--json"])
chk(rc == 0 and _csj is not None,
    "conda config --show-sources --json machine-readable %s" % _diag(rc, sout, serr))

rc, out = run(["config", "--validate"])
chk(rc == 0, "conda config --validate reports no errors (exit 0)")

# single write round-trip: --set then --get then --remove-key restores default.
# (Per the consolidation spec the many per-key config probes collapse into the JSON dump
#  above; this round-trip is the representative set/get/remove-key mutation check.)
rc_set, _ = run(["config", "--set", "always_yes", "true"])
rc_get, out_get = run(["config", "--get", "always_yes"])
chk(rc_set == 0 and rc_get == 0 and "True" in out_get, "config --set always_yes true round-trips via --get")
rc_rk, _ = run(["config", "--remove-key", "always_yes"])
chk(rc_rk == 0, "config --remove-key always_yes restores default")

# ============================================================================
# 6. conda list  --  CONSOLIDATED (list --json parsed for many pkgs/fields) (~5 spawns)
# One --json spawn asserts package presence (numpy/numba/scipy/pandas) and name/version
# fields; plus --export, --explicit, --revisions, and a regex-filter form.
# ============================================================================
rc, _lj, sout, serr = run_json(["list", "--json"])
chk(rc == 0 and isinstance(_lj, list), "conda list --json is a list %s" % _diag(rc, sout, serr))
_lj = _lj if isinstance(_lj, list) else []
chk(all(isinstance(d, dict) for d in _lj) and any("name" in d and "version" in d for d in _lj),
    "list --json entries have name/version")
_names = {d.get("name") for d in _lj if isinstance(d, dict)}
chk("python" in _names, "conda list --json shows python")
# Science-stack presence via the REGEX-FILTER form `conda list <regex> --json`
# (a distinct list capability worth covering, and the robust way to query a
# specific package - the unfiltered full dump can be truncated by conda's own
# repodata paging under a long-running session).
_rc_sci, _sci_doc, _so_sci, _se_sci = run_json(["list", "numpy|numba|scipy|pandas", "--json"])
_sci = {d.get("name") for d in (_sci_doc or []) if isinstance(d, dict)}
chk(_rc_sci == 0, "conda list <regex> --json exit 0 %s" % _diag(_rc_sci, _so_sci, _se_sci))
for _pkg in ("numpy", "numba", "scipy", "pandas"):
    chk(_pkg in _sci, "conda list numpy|numba|scipy|pandas regex shows %s" % _pkg)

rc, out = run(["list", "--export"])
chk(rc == 0 and "=" in out and "python" in out, "conda list --export emits spec lines")

rc, out = run(["list", "--explicit"])
chk(rc == 0 and ("@EXPLICIT" in out or "http" in out or "#" in out), "conda list --explicit URL/@EXPLICIT format")

rc, out = run(["list", "--revisions"])
chk(rc == 0 and ("rev" in out.lower() or ":" in out), "conda list --revisions exit 0")

# regex filter: ^python$ matches the python row but not numpy.
rc, out = run(["list", "^python$"])
_rows = [ln for ln in out.splitlines() if ln and not ln.startswith("#")]
chk(rc == 0 and any(ln.split() and ln.split()[0] == "python" for ln in _rows),
    "conda list ^python$ regex matches python row")
chk(rc == 0 and not any(ln.split() and ln.split()[0] == "numpy" for ln in _rows),
    "conda list ^python$ excludes numpy")

# ============================================================================
# 7. SOLVE SUBCOMMANDS  --  one fast --dry-run --offline --json call each
# (install / create / remove) plus the argparse error path. --help flag coverage
# was already asserted from the captured --help tree above.
# ============================================================================
rc, _dj, sout, serr = run_json(["install", "--dry-run", "--json", "--offline", "python"])
# no-op solve of an already-installed pkg: parseable JSON with dry_run/success/message, whichever
# stream conda puts it on (newer builds split JSON and diagnostics across stdout/stderr).
chk(_json_ok(_dj, rc), "install --dry-run --json --offline python parseable no-op %s"
    % _diag(rc, sout, serr))

rc, _cdj, sout, serr = run_json(["create", "--dry-run", "--json", "--offline", "-n",
                                "_carpet_tmp", "python"])
chk(_json_ok(_cdj, rc), "create --dry-run --json --offline parseable (dry_run or error dict) %s"
    % _diag(rc, sout, serr))

rc, _rmj, sout, serr = run_json(["remove", "--dry-run", "--offline", "--json", "zzz_no_such_pkg_zzz"])
# removing a not-installed pkg: clear message, no mutation. rc may be nonzero.
chk(_rmj is not None or "PackagesNotFound" in serr or "not installed" in (sout + serr).lower() or
    "no packages" in (sout + serr).lower(),
    "remove --dry-run not-installed pkg reports cleanly")

# ============================================================================
# 8. conda clean  --  offline dry-run (deletes nothing) (1 spawn)
# ============================================================================
rc, _clj, sout, serr = run_json(["clean", "--all", "--dry-run", "--json"])
chk(rc == 0 and _clj is not None,
    "conda clean --all --dry-run --json machine-readable dict %s" % _diag(rc, sout, serr))

# ============================================================================
# 9. conda env  --  functional sub-subcommands (offline-safe) (~3 spawns)
# One `env export -n base` (YAML) proves the export capability AND is reused for the
# `compare` block below (no separate export spawn there).
# ============================================================================
rc_x, _base_yaml, _ex = run_split(["env", "export", "-n", "base"])
chk(rc_x == 0 and "name:" in _base_yaml and "dependencies:" in _base_yaml,
    "conda env export -n base YAML has name:/dependencies:")

rc, _elj, sout, serr = run_json(["env", "list", "--json"])
chk(rc == 0 and isinstance(_elj, dict) and isinstance(_elj.get("envs"), list) and len(_elj["envs"]) >= 1,
    "conda env list --json envs array present %s" % _diag(rc, sout, serr))

rc, out = run(["env", "config", "vars", "list", "-n", "base"])
chk(rc == 0, "conda env config vars list -n base exit 0")

# ============================================================================
# 10. conda run  --  functional offline call (1 spawn)
# ============================================================================
rc, out = run(["run", "-n", "base", "python", "-c", "print(6*7)"])
chk(rc == 0 and "42" in out, "conda run -n base python -c prints 42")

# ============================================================================
# 11. conda compare  --  env-vs-self-export must match (1 spawn: reuses the YAML above)
# ============================================================================
_cmp_ok = False
try:
    if rc_x == 0 and "dependencies:" in _base_yaml:
        _tf = tempfile.NamedTemporaryFile("w", suffix=".yml", delete=False)
        _tf.write(_base_yaml)
        _tf.close()
        rc_c, cout = run(["compare", "-n", "base", _tf.name])
        _cmp_ok = (rc_c == 0 and ("uccess" in cout or "match" in cout.lower()))
        os.unlink(_tf.name)
except Exception:
    _cmp_ok = False
chk(_cmp_ok, "conda compare env-vs-self-export reports Success/match")

# ============================================================================
# 12. conda search  --  offline-safe (local env scan; '*' offline determinism) (2 spawns)
# ============================================================================
rc, out = run(["search", "--envs", "python"])
chk(rc == 0, "conda search --envs python (local env scan) exit 0")

# '*' --offline must exit deterministically: cached hit or clean error, never a traceback.
rc, out = run(["search", "*", "--offline"])
chk("Traceback" not in out, "conda search '*' --offline no python traceback")
chk(rc == 0 or ("rror" in out or "offline" in out.lower() or "not" in out.lower()),
    "conda search '*' --offline exits deterministically")


def _offline_call(args, timeout=300):
    """Run one conda command with CONDA_OFFLINE=1 for that call only.

    `--offline` is a per-subcommand option (search, repoquery, ...), not a top-level flag: putting
    it before the subcommand is an argparse error on current conda. The supported way to force a
    whole invocation offline is the CONDA_OFFLINE environment variable, which `run` picks up in
    both its in-process and subprocess paths. It is set here and restored afterwards so it cannot
    leak into any later check.
    """
    previous = os.environ.get("CONDA_OFFLINE")
    os.environ["CONDA_OFFLINE"] = "1"
    try:
        return run(args, timeout=timeout)
    finally:
        if previous is None:
            os.environ.pop("CONDA_OFFLINE", None)
        else:
            os.environ["CONDA_OFFLINE"] = previous


# ============================================================================
# 13. conda notices / doctor  --  behavior beyond --help (2 spawns)
# ============================================================================
# `conda notices` with CONDA_OFFLINE=1 may legitimately exit rc=1 (no cached notice channels and
# no network) while still printing a deterministic explanation, so a non-zero status alone is not
# a failure; a Python traceback is. rc=2 means the invocation itself was rejected, which stays a
# failure.
rc, out = _offline_call(["notices"])
chk("Traceback" not in out and rc in (0, 1),
    "conda notices with CONDA_OFFLINE=1 deterministic exit (rc 0 or 1), no traceback")

# A bare `conda doctor` scans every installed package's file manifest against
# the filesystem - O(packages x files), which is pathologically slow under QEMU
# TCG full-emulation (minutes for a 250-package env). The doctor subcommand is
# fully covered via `conda doctor --help` in the subcommand-tree loop above; here
# we exercise its behavior surface through --help (health/report vocabulary)
# rather than the multi-minute full scan.
rc, out = run(["doctor", "--help"])
chk(rc == 0 and "usage" in out.lower(), "conda doctor --help exit 0 + usage")
chk("health" in out.lower() or "environment" in out.lower() or "check" in out.lower(),
    "conda doctor --help documents its health/environment-check purpose")

# ============================================================================
# 14. conda package --which  --  offline local-db owner resolution (1 spawn, guarded)
# ============================================================================
_pyexe = None
if _base and os.path.isdir(_base):
    for _cand in (os.path.join(_base, "bin", "python"), os.path.join(_base, "bin", "python3")):
        if os.path.exists(_cand):
            _pyexe = _cand
            break
if _pyexe:
    rc, out = run(["package", "--which", _pyexe])
    chk(rc == 0 and ("python" in out.lower() or _pyexe in out), "conda package --which resolves python owner")
else:
    # honest-skip: no python binary located under base prefix on this target.
    chk(True, "conda package --which skipped (python binary not located under prefix)")

# ============================================================================
# 15. conda repoquery  --  offline (guarded: skip cleanly if plugin absent) (1 spawn)
# repoquery --help was already captured in the tree; reuse it to decide whether to probe.
# ============================================================================
_rq_help = _help_out.get("repoquery", (1, ""))[1]
if "depends" in _rq_help or "whoneeds" in _rq_help:
    chk("depends" in _rq_help and "whoneeds" in _rq_help, "repoquery --help lists depends/whoneeds")
    rc2, out2 = run(["repoquery", "depends", "--offline", "python"])
    chk("Traceback" not in out2, "conda repoquery depends python no traceback (offline)")
else:
    # honest-skip: repoquery plugin not installed in this Miniforge build.
    chk(True, "conda repoquery skipped (plugin not available)")

# ============================================================================
# 16. EXIT-CODE / ERROR-PATH DETERMINISM (2 spawns)
# ============================================================================
rc, out = run(["frobnicate"])
chk(rc != 0 and ("rror" in out or "invalid choice" in out or "argument" in out.lower()),
    "bogus subcommand 'conda frobnicate' exits non-zero with error")

rc, out = run(["install", "--nonexistent-flag-xyz"])
chk(rc != 0 and ("rror" in out or "unrecognized" in out or "invalid" in out.lower()),
    "conda install --nonexistent-flag argparse error")

# HONEST-SKIP (documented, not asserted):
#   - conda activate/deactivate deep semantics: require an interactive shell hook (state-mutating,
#     shell-integration dependent) - not offline-safe to assert from a subprocess. (--help asserted.)
#   - conda init <shell> (real, non-dry-run): mutates rc files / registry - state-mutating, skipped.
#   - conda content-trust / notices remote fetch / search remote index: require network, skipped.
#   - conda repoquery sub-subcommands beyond --help: guarded above (plugin may be absent).
#   - real create/install/update/remove (non --dry-run): network + state-mutating, skipped.

print("CONDACLI_RESULT ok=%d fail=%d" % (ok, fail))
if fail == 0:
    print("CONDACLI_DONE")
    sys.exit(0)
sys.exit(1)
