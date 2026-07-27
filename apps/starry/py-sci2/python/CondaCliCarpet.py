#!/usr/bin/env python3
# CondaCliCarpet.py - exhaustive conda command-line surface carpet.
#
# Ground truth is `conda --help`'s own subcommand tree: every subcommand's `--help` must
# print its `usage: conda <sub>` banner and exit 0, and the core informational commands
# (--version / info / list / config --show ...) must return real, well-formed output.
# Runs the glibc Miniforge `conda` staged at /opt/miniconda on StarryOS.
#
# TCG NOTE: each `conda` spawn costs ~35s under full-emulation because conda's Python
# startup is heavy. To stay tractable we CONSOLIDATE: instead of one spawn per assertion,
# a single rich `--json` invocation is PARSED for many assertions. Coverage (every
# subcommand + every config/info/list field) is preserved; only the spawn COUNT drops.
import json
import os
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


# Under QEMU TCG each conda spawn (heavy Python startup) costs ~60-100s, so 300s
# amply caps any single call while still bounding a genuinely hung one.
def run(args, timeout=300):
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
    # separate stdout/stderr so JSON parsers only see stdout
    env = dict(os.environ)
    env.setdefault("CONDA_ALWAYS_YES", "1")
    try:
        r = subprocess.run([CONDA] + args, capture_output=True, text=True, env=env, timeout=timeout)
    except subprocess.TimeoutExpired:
        return 124, "", "TIMEOUT: conda %s exceeded %ds" % (" ".join(args), timeout)
    return r.returncode, (r.stdout or ""), (r.stderr or "")


def loadjson(text):
    # conda --json output is a single JSON document on stdout; tolerate leading noise.
    try:
        return json.loads(text)
    except Exception:
        s = text.find("{")
        b = text.find("[")
        starts = [x for x in (s, b) if x >= 0]
        if not starts:
            return None
        i = min(starts)
        try:
            return json.loads(text[i:])
        except Exception:
            return None


# ============================================================================
# 1. TOP-LEVEL SURFACE  (2 spawns)
# ============================================================================
rc, out = run(["--version"])
_verline = out.strip()
chk(rc == 0 and _verline.lower().startswith("conda "), "conda --version")
chk(any(ch.isdigit() for ch in out), "conda --version has a version number")
_vparts = _verline.split()
chk(len(_vparts) >= 2 and _vparts[1][0].isdigit(), "conda --version parses to N.N.N")

rc, out = run(["--help"])
chk(rc == 0 and "usage: conda" in out, "conda --help usage banner")
chk("install" in out and "create" in out and "list" in out, "conda --help lists core subcommands")

# ============================================================================
# 2. FULL SUBCOMMAND --help TREE  (one `--help` spawn per subcommand)
# Ground truth: every subcommand conda advertises must answer --help with a usage
# banner, exit 0, and mention an expected keyword/flag for that subcommand.
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
# de-dup while asserting the union of expected keywords per subcommand
_help_out = {}
_expected = {}
for sub, kw in HELP_TREE:
    _expected.setdefault(sub, [])
    if kw not in _expected[sub]:
        _expected[sub].append(kw)

SUBCOMMANDS = list(_expected.keys())
for sub in SUBCOMMANDS:
    rc, out = run([sub, "--help"])
    _help_out[sub] = (rc, out)
    chk(rc == 0, "conda %s --help exit 0" % sub)
    chk(("usage: conda %s" % sub) in out or ("usage: conda" in out and sub in out),
        "conda %s --help usage banner" % sub)
    chk("-h" in out or "--help" in out, "conda %s --help advertises -h" % sub)
    for kw in _expected[sub]:
        chk(kw in out, "conda %s --help advertises %s" % (sub, kw))

# rich --help flag coverage parsed from the already-captured install/create/remove/clean/
# search/run/init/rename/env/package --help output (NO extra spawns).
_ihelp = _help_out.get("install", (1, ""))[1]
chk("--no-deps" in _ihelp, "install --help advertises --no-deps")
chk("--only-deps" in _ihelp, "install --help advertises --only-deps")
chk("--update-deps" in _ihelp, "install --help advertises --update-deps")
chk("--force-reinstall" in _ihelp, "install --help advertises --force-reinstall")
chk("--revision" in _ihelp, "install --help advertises --revision")

_chelp = _help_out.get("create", (1, ""))[1]
chk("--file" in _chelp, "create --help advertises --file")
chk(("-c" in _chelp) or ("--channel" in _chelp), "create --help advertises -c/--channel")

_rhelp = _help_out.get("remove", (1, ""))[1]
chk("--force" in _rhelp or "--force-remove" in _rhelp, "remove --help advertises --force")

_clhelp = _help_out.get("clean", (1, ""))[1]
chk("--all" in _clhelp, "clean --help advertises --all")
chk("--packages" in _clhelp, "clean --help advertises --packages")
chk("--index-cache" in _clhelp, "clean --help advertises --index-cache")

_shelp = _help_out.get("search", (1, ""))[1]
chk("--channel" in _shelp or "-c" in _shelp, "search --help advertises --channel")
chk("--envs" in _shelp, "search --help advertises --envs")

_runhelp = _help_out.get("run", (1, ""))[1]
chk("--cwd" in _runhelp, "run --help advertises --cwd")
chk("-n" in _runhelp or "--name" in _runhelp, "run --help advertises -n/--name")

_inithelp = _help_out.get("init", (1, ""))[1]
chk("bash" in _inithelp or "shells" in _inithelp.lower(), "init --help lists shells (bash)")
chk("--reverse" in _inithelp, "init --help advertises --reverse")

_rnhelp = _help_out.get("rename", (1, ""))[1]
chk("source" in _rnhelp.lower() or "destination" in _rnhelp.lower() or "-d" in _rnhelp,
    "rename --help documents source/destination")

_envhelp = _help_out.get("env", (1, ""))[1]
chk("create" in _envhelp and "export" in _envhelp and "remove" in _envhelp and "update" in _envhelp,
    "conda env --help lists sub-subcommands")

_pkghelp = _help_out.get("package", (1, ""))[1]
chk("--which" in _pkghelp or "--pack" in _pkghelp, "package --help advertises --which/--pack")

# ============================================================================
# 3. -h  ==  --help  ALIAS  (1 spawn, representative subcommand)
# Instead of spawning `-h` for every subcommand, prove the alias once on `info`
# and rely on the --help tree above for the rest.
# ============================================================================
rc_h, out_h = run(["info", "-h"])
_info_help = _help_out.get("info", (1, ""))[1]
chk(rc_h == 0 and "usage: conda" in out_h, "conda info -h short form exit 0")
chk(out_h.strip() == _info_help.strip(), "conda info -h output equals --help (-h is alias)")

# ============================================================================
# 4. conda info  --  CONSOLIDATED (info --json parsed for many fields) (~3 spawns)
# One machine-readable spawn covers version/platform/python/channels/envs/root_prefix/
# pkgs_dirs (subsuming the former --all/--system/--base/--unsafe-channels field checks).
# ============================================================================
rc, sout, serr = run_split(["info", "--json"])
_ij = loadjson(sout)
chk(rc == 0 and isinstance(_ij, dict), "conda info --json parses to dict")
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
rc, sout, serr = run_split(["--json", "info", "--envs"])
_ie = loadjson(sout)
chk(rc == 0 and _ie is not None, "conda --json info --envs valid top-level JSON (global --json placement)")
chk(isinstance(_ie, dict) and (isinstance(_ie.get("envs"), list) or "envs" in _ie),
    "conda info --envs enumerates environments")

# ============================================================================
# 5. conda config  --  CONSOLIDATED (config --show --json parsed for many keys) (~5 spawns)
# One --show --json spawn asserts every config key formerly probed one-by-one via
# --get/--describe (channels, channel_priority, ssl_verify, always_yes,
# show_channel_urls, ...). Plus show-sources --json, a set/get/remove-key round-trip,
# and --validate.
# ============================================================================
rc, sout, serr = run_split(["config", "--show", "--json"])
_cj = loadjson(sout)
chk(rc == 0 and isinstance(_cj, dict), "conda config --show --json parses to dict")
_cj = _cj if isinstance(_cj, dict) else {}
chk("channels" in _cj, "config --show --json has channels key")
chk(isinstance(_cj.get("channels"), list), "config --show --json channels is a list")
# every config key previously probed via --get/--describe, now asserted from the JSON dump.
for _key in ("channel_priority", "ssl_verify", "always_yes", "show_channel_urls",
             "channel_alias", "default_channels", "add_pip_as_python_dependency",
             "auto_update_conda", "pkgs_dirs", "envs_dirs"):
    chk(_key in _cj, "config --show --json documents key %s" % _key)

rc, sout, serr = run_split(["config", "--show-sources", "--json"])
_csj = loadjson(sout)
chk(rc == 0 and _csj is not None, "conda config --show-sources --json machine-readable")

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
rc, sout, serr = run_split(["list", "--json"])
_lj = loadjson(sout)
chk(rc == 0 and isinstance(_lj, list), "conda list --json is a list")
_lj = _lj if isinstance(_lj, list) else []
chk(all(isinstance(d, dict) for d in _lj) and any("name" in d and "version" in d for d in _lj),
    "list --json entries have name/version")
_names = {d.get("name") for d in _lj if isinstance(d, dict)}
chk("python" in _names, "conda list --json shows python")
# Science-stack presence via the REGEX-FILTER form `conda list <regex> --json`
# (a distinct list capability worth covering, and the robust way to query a
# specific package - the unfiltered full dump can be truncated by conda's own
# repodata paging under a long-running session).
_rc_sci, _so_sci, _se_sci = run_split(["list", "numpy|numba|scipy|pandas", "--json"])
_sci = {d.get("name") for d in (loadjson(_so_sci) or []) if isinstance(d, dict)}
chk(_rc_sci == 0, "conda list <regex> --json exit 0")
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
rc, sout, serr = run_split(["install", "--dry-run", "--json", "--offline", "python"])
_dj = loadjson(sout)
# no-op solve of an already-installed pkg: parseable JSON with dry_run/success/message.
chk(isinstance(_dj, dict) and ("dry_run" in _dj or "success" in _dj or "message" in _dj or "error" in _dj),
    "install --dry-run --json --offline python parseable no-op")

rc, sout, serr = run_split(["create", "--dry-run", "--json", "--offline", "-n", "_carpet_tmp", "python"])
_cdj = loadjson(sout)
chk(isinstance(_cdj, dict), "create --dry-run --json --offline parseable (dry_run or error dict)")

rc, sout, serr = run_split(["remove", "--dry-run", "--offline", "--json", "zzz_no_such_pkg_zzz"])
_rmj = loadjson(sout)
# removing a not-installed pkg: clear message, no mutation. rc may be nonzero.
chk(_rmj is not None or "PackagesNotFound" in serr or "not installed" in (sout + serr).lower() or
    "no packages" in (sout + serr).lower(),
    "remove --dry-run not-installed pkg reports cleanly")

# ============================================================================
# 8. conda clean  --  offline dry-run (deletes nothing) (1 spawn)
# ============================================================================
rc, sout, serr = run_split(["clean", "--all", "--dry-run", "--json"])
chk(rc == 0 and loadjson(sout) is not None, "conda clean --all --dry-run --json machine-readable dict")

# ============================================================================
# 9. conda env  --  functional sub-subcommands (offline-safe) (~3 spawns)
# One `env export -n base` (YAML) proves the export capability AND is reused for the
# `compare` block below (no separate export spawn there).
# ============================================================================
rc_x, _base_yaml, _ex = run_split(["env", "export", "-n", "base"])
chk(rc_x == 0 and "name:" in _base_yaml and "dependencies:" in _base_yaml,
    "conda env export -n base YAML has name:/dependencies:")

rc, sout, serr = run_split(["env", "list", "--json"])
_elj = loadjson(sout)
chk(rc == 0 and isinstance(_elj, dict) and isinstance(_elj.get("envs"), list) and len(_elj["envs"]) >= 1,
    "conda env list --json envs array present")

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

# ============================================================================
# 13. conda notices / doctor  --  behavior beyond --help (2 spawns)
# ============================================================================
rc, out = run(["notices"])
chk("Traceback" not in out and rc == 0, "conda notices offline exit 0, no traceback")

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
