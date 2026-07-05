#!/usr/bin/env python3
# run_monitor.py -- on-target gate for the StarryOS `monitor` app: the Prometheus monitoring stack
# (prometheus + promtool + node_exporter) and the glances system monitor (CLI / headless / TUI /
# client-server / web).
#
# Runs each carpet as its OWN subprocess; a carpet counts as PASS only when its DONE marker is
# present, it exited 0, AND no `  FAIL ` line appears in its output. Prints `MONITOR_OK=<P>/<T>` and
# `TEST PASSED` only when EVERY carpet passes, else `TEST FAILED`. The PASS/FAIL anchor lives ONLY
# here so the harness success_regex can never self-match a launch command echoed on the console.
import os, subprocess, sys

HERE = os.path.dirname(os.path.abspath(__file__))
PROGRAMS = os.path.normpath(os.path.join(HERE, "..", "programs"))
os.chdir(HERE)

PY = sys.executable
env0 = dict(os.environ)
# make pty_tui_drive / pyte_assert importable by the TUI carpet subprocess.
env0["PYTHONPATH"] = os.pathsep.join(
    [p for p in (PROGRAMS, HERE, env0.get("PYTHONPATH", "")) if p])
env0.setdefault("PYTHONDONTWRITEBYTECODE", "1")
env0.setdefault("PYTHONUNBUFFERED", "1")
env0.setdefault("GLANCES_BIN", "glances")

# (name, module, DONE marker). Fast/deterministic first; the Go stack and the TUI soak last.
CARPETS = [
    ("glances-cli",      "GlancesCliCarpet.py",      "GCLI_DONE"),
    ("glances-headless", "GlancesHeadlessCarpet.py", "GHDL_DONE"),
    ("node_exporter",    "NodeExporterCarpet.py",    "NE_DONE"),
    ("grafana",          "GrafanaCarpet.py",         "GRAF_DONE"),
    ("glances-cs",       "GlancesCsCarpet.py",       "GCS_DONE"),
    ("glances-web",      "GlancesWebCarpet.py",      "GWEB_DONE"),
    ("prometheus",       "PrometheusCarpet.py",      "PROM_DONE"),
    ("glances-tui",      "GlancesTuiCarpet.py",      "GTUI_DONE"),
]

print("=== python3 %s ===" % sys.version.split()[0])
print("=== monitor: prometheus + node_exporter + grafana + glances (cli|headless|tui|client-server|web) carpets ===")

passed = 0
for name, fn, marker in CARPETS:
    print("\n----- %s (%s) -----" % (name, fn))
    r = subprocess.run([PY, os.path.join(HERE, fn)], capture_output=True, text=True, env=env0)
    out = (r.stdout or "") + (r.stderr or "")
    result = [ln for ln in out.splitlines() if "_RESULT ok=" in ln]
    ok = marker in out and r.returncode == 0 and "  FAIL " not in out
    # echo the carpet's own lines so the console log shows the evidence.
    for ln in out.splitlines():
        print("  | " + ln)
    if ok:
        print("  OK   %s (%s)" % (name, result[0].strip() if result else ""))
        passed += 1
    else:
        print("  FAIL %s (marker %s, rc=%s)" % (name, marker, r.returncode))

total = len(CARPETS)
print("\nMONITOR_RESULT PASS=%d TOTAL=%d" % (passed, total))
print("MONITOR_OK=%d/%d" % (passed, total))
if passed == total and total > 0:
    print("TEST PASSED")
    sys.exit(0)
print("TEST FAILED")
sys.exit(1)
