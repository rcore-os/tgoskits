#!/usr/bin/env python3
# GlancesHeadlessCarpet.py -- glances one-shot NON-curses stdout path. Exercises the standalone
# GlancesStdout interface (serve_n(1): one refresh, no curses) and asserts the psutil-read numbers
# are real (mem.total a plausible RAM size, cpu has the canonical fields, load present), plus the
# machine-readable --stdout-json path. This proves glances reads StarryOS /proc correctly, not just
# "it didn't crash". It also asserts the psutil plugins backed by the newer procfs sources --
# network (/proc/net/dev), diskio (/proc/diskstats) and fs (/proc/mounts + statfs) -- return real,
# non-empty stats: the loopback interface with a byte counter, the root virtio-blk disk "vda", and
# the ext4 root filesystem with a non-zero size. These are the same three left-sidebar sections the
# curses TUI carpet renders.
#
# Emits `GHDL_RESULT ok=<N> fail=<F>` and, only when F==0, `GHDL_DONE`.
import ast, json, os, re, socket, subprocess, sys

GLANCES = os.environ.get("GLANCES_BIN", "glances")
DISABLE = "ip,cloud,containers,docker,folders,ports,smart,wifi,gpu,connections,sensors"
_ok = 0
_fail = 0


def warm_loopback(nbytes=131072):
    """Push real traffic through the loopback interface so /proc/net/dev's `lo`
    byte counters are non-zero when the snapshot is taken.

    The kernel counts loopback bytes only when packets actually flow (the router
    increments lo rx/tx on the loopback egress fast path); an idle boot leaves
    them at 0. This carpet runs before any server carpet binds loopback, so
    without self-generated traffic `lo` would legitimately read 0. Generating a
    deterministic loopback transfer here makes the `lo bytes > 0` assertion a
    self-contained proof that the counters are live -- the network analogue of
    the diskio (boot reads) and fs (statfs) proofs below."""
    rx = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    rx.bind(("127.0.0.1", 0))
    rx.settimeout(0.2)
    tx = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    addr = rx.getsockname()
    payload = b"x" * 1024
    sent = 0
    while sent < nbytes:
        tx.sendto(payload, addr)
        sent += len(payload)
        try:
            rx.recvfrom(2048)
        except socket.timeout:
            pass
    tx.close()
    rx.close()


def check(cond, label):
    global _ok, _fail
    if cond:
        _ok += 1
    else:
        _fail += 1
        print("  FAIL %s" % label)


def run(extra, timeout=90):
    args = [GLANCES] + extra + ["--time", "1", "--stop-after", "1",
                                "--disable-check-update", "--disable-plugin", DISABLE]
    r = subprocess.run(args, capture_output=True, text=True, timeout=timeout)
    return r.returncode, (r.stdout or ""), (r.stderr or "")


def main():
    print("=== GlancesHeadlessCarpet: one-shot --stdout snapshot (psutil /proc read) ===")

    # 1. cpu,mem,load whole-plugin snapshot -> three plugin dict lines.
    rc, out, err = run(["--stdout", "cpu,mem,load"])
    check(rc == 0, "`--stdout cpu,mem,load` exits 0")
    print(out.strip()[:400])
    cpu_line = next((l for l in out.splitlines() if l.startswith("cpu:")), None)
    mem_line = next((l for l in out.splitlines() if l.startswith("mem:")), None)
    load_line = next((l for l in out.splitlines() if l.startswith("load:")), None)
    check(cpu_line is not None, "cpu plugin line printed")
    check(mem_line is not None, "mem plugin line printed")
    check(load_line is not None, "load plugin line printed")

    # parse the python-dict literals glances prints and assert field shape + plausibility.
    def parse(line):
        try:
            return ast.literal_eval(line.split(":", 1)[1].strip())
        except Exception:
            return None

    cpu = parse(cpu_line) if cpu_line else None
    mem = parse(mem_line) if mem_line else None
    load = parse(load_line) if load_line else None
    check(isinstance(cpu, dict) and all(k in cpu for k in ("total", "user", "system", "idle")),
          "cpu dict has canonical fields total/user/system/idle")
    check(isinstance(cpu, dict) and "cpucore" in cpu and int(cpu["cpucore"]) >= 1,
          "cpu.cpucore >= 1 (real core count)")
    check(isinstance(mem, dict) and all(k in mem for k in ("total", "available", "percent", "used")),
          "mem dict has canonical fields total/available/percent/used")
    check(isinstance(mem, dict) and int(mem.get("total", 0)) > (1 << 20),
          "mem.total > 1 MiB (psutil really read /proc/meminfo MemTotal, not a 0 fallback)")
    check(isinstance(load, dict) and "min1" in load and "cpucore" in load,
          "load dict has min1 + cpucore")

    # 2. attribute form: a clean machine-checkable mem.total + cpu.total.
    rc, out, err = run(["--stdout", "mem.total,cpu.total"])
    check(rc == 0, "`--stdout mem.total,cpu.total` exits 0")
    m = re.search(r"^mem\.total:\s*([0-9]+)\s*$", out, re.M)
    check(bool(m) and int(m.group(1)) > (1 << 20), "mem.total attribute is an integer > 1 MiB")
    c = re.search(r"^cpu\.total:\s*([0-9.]+)\s*$", out, re.M)
    check(bool(c) and 0.0 <= float(c.group(1)) <= 100.0, "cpu.total attribute is a 0..100 percent")

    # 3. JSON stdout path (machine-consumable) -> valid JSON, plugin nested under its name:
    #    `--stdout-json mem` -> {"mem": {"total": ..., ...}}.
    rc, out, err = run(["--stdout-json", "mem"])
    check(rc == 0, "`--stdout-json mem` exits 0")
    try:
        blob = out[out.index("{"):out.rindex("}") + 1]
        obj = json.loads(blob)
        memobj = obj.get("mem", obj) if isinstance(obj, dict) else {}
        check(isinstance(memobj, dict) and int(memobj.get("total", 0)) > (1 << 20),
              "--stdout-json mem is valid JSON with mem.total > 1 MiB")
    except Exception as e:
        check(False, "--stdout-json mem parses as JSON (%r)" % e)

    # 4. procfs-backed plugins: network (/proc/net/dev), diskio (/proc/diskstats),
    #    fs (/proc/mounts + statfs). Each `--stdout <plugin>` prints `<plugin>: [ {..}, .. ]` -- one
    #    dict per interface / disk / mount. Assert the lists are non-empty AND carry the specific
    #    StarryOS entities with real values (not an empty list that a missing /proc source would give).
    # Drive deterministic loopback traffic first so `lo` has live byte counters (see warm_loopback).
    warm_loopback()
    rc, out, err = run(["--stdout", "network,diskio,fs"])
    check(rc == 0, "`--stdout network,diskio,fs` exits 0")

    def parse_list(prefix):
        line = next((l for l in out.splitlines() if l.startswith(prefix + ":")), None)
        if line is None:
            return None
        try:
            v = ast.literal_eval(line.split(":", 1)[1].strip())
            return v if isinstance(v, list) else None
        except Exception:
            return None

    # network: /proc/net/dev -> at least the loopback interface, with a cumulative byte gauge.
    net = parse_list("network")
    check(isinstance(net, list) and len(net) > 0, "network plugin returns a non-empty interface list")
    ifaces = {d.get("interface_name") for d in net} if net else set()
    print("  network interfaces: %s" % sorted(x for x in ifaces if x))
    lo = next((d for d in (net or []) if d.get("interface_name") == "lo"), None)
    check(lo is not None, "network plugin exposes the 'lo' loopback interface (from /proc/net/dev)")
    check(lo is not None and int(lo.get("bytes_all_gauge", 0)) > 0,
          "lo has a non-zero cumulative byte counter (bytes_all_gauge > 0): %r"
          % (lo.get("bytes_all_gauge") if lo else None))

    # diskio: /proc/diskstats -> the root virtio-blk disk 'vda' with cumulative read counters.
    dio = parse_list("diskio")
    check(isinstance(dio, list) and len(dio) > 0, "diskio plugin returns a non-empty disk list")
    disks = {d.get("disk_name") for d in dio} if dio else set()
    print("  diskio disks: %s" % sorted(x for x in disks if x))
    vda = next((d for d in (dio or []) if d.get("disk_name") == "vda"), None)
    check(vda is not None, "diskio plugin exposes the 'vda' root disk (from /proc/diskstats)")
    # glances flags the diskio byte/count fields rate=True: the cumulative /proc/diskstats counter is
    # carried in `<field>_gauge`, while the bare `<field>` is only the delta since the previous
    # refresh (legitimately 0 when no I/O lands in the sub-second sampling window). The boot-reads
    # proof is the cumulative gauge -- the diskio analogue of the network plugin's bytes_all_gauge.
    read_bytes = int(vda.get("read_bytes_gauge", vda.get("read_bytes", 0))) if vda else 0
    check(read_bytes > 0,
          "vda has non-zero cumulative read_bytes (boot reads): %r" % (read_bytes if vda else None))

    # fs: /proc/mounts + statfs -> the ext4 root filesystem with a real total size.
    fs = parse_list("fs")
    check(isinstance(fs, list) and len(fs) > 0, "fs plugin returns a non-empty filesystem list")
    mounts = {d.get("mnt_point") for d in fs} if fs else set()
    print("  fs mount points: %s" % sorted(x for x in mounts if x))
    root = next((d for d in (fs or []) if d.get("mnt_point") == "/"), None)
    check(root is not None, "fs plugin exposes the '/' root mount (from /proc/mounts)")
    check(root is not None and int(root.get("size", 0)) > 0,
          "root filesystem has a non-zero total size from statfs: %r" % (root.get("size") if root else None))

    print("GHDL_RESULT ok=%d fail=%d" % (_ok, _fail))
    if _fail == 0:
        print("GHDL_DONE")
        return 0
    return 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as e:
        import traceback
        traceback.print_exc()
        print("GHDL_RESULT ok=%d fail=%d" % (_ok, _fail + 1))
        sys.exit(1)
