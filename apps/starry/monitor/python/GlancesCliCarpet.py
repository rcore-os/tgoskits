#!/usr/bin/env python3
# GlancesCliCarpet.py -- glances CLI surface carpet: version RED-LINE + exhaustive `--help` option
# tree + module list + shell-completion. Every documented option/mode is asserted present against
# the real `glances --help` output (ground truth = the --help tree of glances 4.4.1), not a subset.
#
# Emits `GCLI_RESULT ok=<N> fail=<F>` and, only when F==0, `GCLI_DONE`.
import os, re, subprocess, sys

GLANCES = os.environ.get("GLANCES_BIN", "glances")
PY = sys.executable
_ok = 0
_fail = 0


def check(cond, label):
    global _ok, _fail
    if cond:
        _ok += 1
    else:
        _fail += 1
        print("  FAIL %s" % label)


def run(args, timeout=60):
    r = subprocess.run([GLANCES] + args, capture_output=True, text=True, timeout=timeout)
    return r.returncode, (r.stdout or "") + (r.stderr or "")


def main():
    print("=== GlancesCliCarpet: version red-line + exhaustive --help tree ===")

    # --- VERSION RED-LINE: glances pinned to 4.4.1; API version 4; psutil present (version-flexible
    #     because the musl apk and a host glibc psutil differ). Wrong glances version = invalid test.
    rc, out = run(["--version"])
    check(rc == 0, "`glances --version` exits 0")
    m = re.search(r"Glances version:\s+([0-9]+\.[0-9]+\.[0-9]+)", out)
    check(bool(m), "`--version` prints a Glances version line")
    check(bool(m) and m.group(1) == "4.4.1", "Glances version RED-LINE == 4.4.1 (got %s)"
          % (m.group(1) if m else None))
    check(re.search(r"Glances API version:\s+4", out) is not None, "Glances API version == 4")
    check(re.search(r"PsUtil version:\s+[0-9]+\.[0-9]+", out) is not None, "PsUtil version line present")
    # -V short form
    rc2, out2 = run(["-V"])
    check(rc2 == 0 and "Glances version:" in out2, "`glances -V` short form works")

    # --- EXHAUSTIVE --help OPTION TREE: assert every option of the DELIVERED build (Alpine glances
    #     4.4.1-r1) is listed. Ground truth = the actual `glances --help` of the musl apk (captured
    #     under qemu-user-static from the delivery closure), NOT a host pip build. `--print-completion`
    #     is deliberately NOT here: it is shtab-gated and Alpine glances 4.4.1-r1 ships without shtab,
    #     so the option is legitimately absent -> handled as a capability probe below, never a red-line.
    rc, help_out = run(["--help"])
    check(rc == 0, "`glances --help` exits 0")
    REQUIRED_OPTS = [
        # short forms
        "-h", "-V", "-d", "-C", "-P", "-0", "-1", "-2", "-3", "-4", "-5", "-6",
        "-c", "-s", "-p", "-B", "-u", "-t", "-w", "-q", "-f", "-b",
        # long forms (the full delivered 4.4.1-r1 set)
        "--help", "--version", "--debug", "--config", "--plugins", "--modules-list", "--module-list",
        "--disable-plugin", "--disable-plugins", "--disable", "--enable-plugin", "--enable-plugins",
        "--enable", "--disable-process", "--disable-webui", "--light", "--enable-light",
        "--disable-irix", "--percpu", "--per-cpu", "--disable-left-sidebar", "--disable-quicklook",
        "--full-quicklook", "--disable-top", "--meangpu", "--disable-history", "--disable-bold",
        "--disable-bg", "--enable-irq", "--enable-process-extended", "--disable-separator",
        "--disable-cursor", "--sort-processes", "--programs", "--program", "--export",
        "--export-csv-file", "--export-csv-overwrite", "--export-json-file", "--export-graph-path",
        "--export-process-filter", "--client", "--server", "--browser", "--disable-autodiscover",
        "--port", "--bind", "--username", "--password", "--snmp-community", "--snmp-port",
        "--snmp-version", "--snmp-user", "--snmp-auth", "--snmp-force", "--time", "--webserver",
        "--cached-time", "--stop-after", "--open-web-browser", "--quiet", "--process-filter",
        "--process-short-name", "--process-long-name", "--stdout", "--stdout-json", "--stdout-csv",
        "--issue", "--trace-malloc", "--memory-leak", "--api-doc", "--api-restful-doc",
        "--hide-kernel-threads", "--byte", "--diskio-show-ramfs", "--diskio-iops", "--diskio-latency",
        "--fahrenheit", "--fs-free-space", "--sparkline", "--disable-unicode", "--hide-public-info",
        "--disable-check-update", "--strftime", "--fetch", "--fetch-template", "--stdout-fetch",
        "--stdout-fetch-template",
    ]
    missing = [o for o in REQUIRED_OPTS if o not in help_out]
    check(not missing, "`--help` lists all %d delivered-build options (missing: %s)"
          % (len(REQUIRED_OPTS), missing))

    # the FIVE run modes glances documents (standalone is the default; the other four are flags).
    for mode_flag, name in [("--client", "client"), ("--server", "server"),
                            ("--webserver", "web server"), ("--browser", "browser"),
                            ("--stdout", "stdout")]:
        check(mode_flag in help_out, "run-mode flag %s (%s) documented in --help" % (mode_flag, name))

    # --sort-processes choices are the canonical psutil sort keys.
    for choice in ["cpu_percent", "memory_percent", "username", "cpu_times", "io_counters", "name"]:
        check(choice in help_out, "--sort-processes choice %s documented" % choice)

    # default client/server port red-line.
    check("61209" in help_out, "default client/server port 61209 documented")

    # --- --modules-list: enumerates plugins + exporters (rc 0, mentions core plugins).
    rc, out = run(["--modules-list"])
    check(rc == 0, "`glances --modules-list` exits 0")
    check(all(p in out for p in ("cpu", "mem", "load", "network")),
          "--modules-list enumerates core plugins (cpu/mem/load/network)")

    # --- shell completion is CAPABILITY-AWARE. glances only offers `--print-completion` when the
    #     optional `shtab` dependency is importable; the delivery build (Alpine glances 4.4.1-r1)
    #     ships WITHOUT shtab (it is not in the apk closure), so the option is legitimately absent.
    #     If a build DOES offer it, assert the generator emits a script (gate on CONTENT, not rc:
    #     shtab exits non-zero on some builds even after printing the script). If absent -> a
    #     documented capability SKIP, never a failure (据实按交付版本, not the host build).
    if "--print-completion" in help_out:
        rc, out = run(["--print-completion", "bash"])
        check(len(out) > 20 and ("shtab" in out or "_glances" in out or "complete" in out.lower()),
              "`--print-completion bash` emits a completion script (shtab present in this build)")
    else:
        print("  SKIP --print-completion -- shtab not bundled in this glances build (Alpine 4.4.1-r1); "
              "the option is not offered (capability absent by design, not a failure)")

    print("GCLI_RESULT ok=%d fail=%d" % (_ok, _fail))
    if _fail == 0:
        print("GCLI_DONE")
        return 0
    return 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as e:
        import traceback
        traceback.print_exc()
        print("GCLI_RESULT ok=%d fail=%d" % (_ok, _fail + 1))
        sys.exit(1)
