#!/usr/bin/env python3
# NodeExporterCarpet.py -- industrial CLI + live carpet for node_exporter 1.11.1 on StarryOS.
# node_exporter is a fully-static, CGO-disabled Go binary (the same target-arch build the Prometheus
# carpet scrapes at /usr/bin/node_exporter). It has NO subcommands: its whole surface is one flat
# kingpin flag tree (`node_exporter [<flags>]`) plus a Prometheus /metrics HTTP endpoint. This carpet
# grounds every assertion in the tool's OWN `--help` output (exit 0) -- the exact, complete flag
# surface, which is arch-independent, so the x86 ground truth applies to every arch the overlay ships.
#
# DIMENSIONS (all real, gated PASS/TOTAL -- no silent pass):
#   VER       -- `--version` first line is EXACTLY `node_exporter, version 1.11.1 ...` (red-line).
#   HELP      -- `--help` exits 0 and documents the general/web/path/log flags with their EXACT
#                printed defaults (proves we read the real help, not an invented surface).
#   COLLECTOR -- `--help` enumerates ALL 77 `--[no-]collector.<name>` enable toggles (48 enabled +
#                29 disabled by default); assert a broad representative subset AND exact set-equality
#                to the documented 77, plus a spread of per-collector configuration flags + defaults.
#   REJECT    -- bad enum values and invented flags are REJECTED with rc!=0 (the parser validates its
#                own surface -- this is the guard that keeps every asserted flag a REAL one).
#   LIVE      -- start the exporter headless on loopback with a curated collector set whose data comes
#                from the /proc files StarryOS renders: cpu/stat (/proc/stat), meminfo (/proc/meminfo),
#                loadavg (/proc/loadavg), vmstat (/proc/vmstat), netdev (/proc/net/dev),
#                diskstats (/proc/diskstats), filesystem (/proc/mounts + statfs) plus the pure-syscall
#                collectors uname/time. GET the metrics path and assert the documented node_* families
#                are exposed AND carry real, non-zero values: node_cpu_seconds_total, node_memory_*,
#                node_load1, node_vmstat_pgfault (page-fault counter), node_network_receive_bytes_total
#                {device="lo"} (loopback scrape traffic), node_disk_reads_completed_total{device="vda"}
#                (root virtio-blk), and node_filesystem_size_bytes{mountpoint="/"} (ext4 root via
#                statfs). A custom --web.telemetry-path is honored (moved off /metrics) and the landing
#                page is served; then stop it.
#
# The vast majority of collectors (nvme/zfs/systemd/perf/ntp/supervisord/...) drive hardware, daemons
# or subsystems that do not exist on-target, so their flag surface is asserted from `--help` only --
# the help tree is the arch/kernel-independent ground truth. The live run enables exactly the /proc-
# and syscall-backed collectors that StarryOS genuinely serves.
#
# Exercises the Go runtime (M:N scheduler, GC, futex, getrandom), the loopback net stack
# (bind/listen/accept), block-device I/O accounting and the procfs reads on the StarryOS image. Emits
# `NE_RESULT ok=<N> fail=<F>` and, only when F==0, `NE_DONE`.
import os, re, signal, subprocess, sys, time, urllib.request, urllib.error

NODE_EXP = os.environ.get("NODE_EXPORTER_BIN", "/usr/bin/node_exporter")
PORT = int(os.environ.get("MONITOR_NODE_EXPORTER_PORT", "19100"))
HOST = "127.0.0.1"
TELEMETRY = "/nodemetrics"  # deliberately NOT /metrics, to prove --web.telemetry-path is honored.
VER = "1.11.1"
_ok = 0
_fail = 0
_OPENER = urllib.request.build_opener(urllib.request.ProxyHandler({}))

# The complete, documented set of `--[no-]collector.<name>` enable toggles (node_exporter 1.11.1).
COLLECTORS_ENABLED = [
    "arp", "bcache", "bcachefs", "bonding", "btrfs", "conntrack", "cpu", "cpufreq", "diskstats",
    "dmi", "edac", "entropy", "fibrechannel", "filefd", "filesystem", "hwmon", "infiniband", "ipvs",
    "kernel_hung", "loadavg", "mdadm", "meminfo", "netclass", "netdev", "netstat", "nfs", "nfsd",
    "nvme", "os", "powersupplyclass", "pressure", "rapl", "schedstat", "selinux", "sockstat",
    "softnet", "stat", "tapestats", "textfile", "thermal_zone", "time", "timex", "udp_queues",
    "uname", "vmstat", "watchdog", "xfs", "zfs",
]
COLLECTORS_DISABLED = [
    "buddyinfo", "cgroups", "cpu_vulnerabilities", "drbd", "drm", "ethtool", "interrupts", "ksmd",
    "lnstat", "logind", "meminfo_numa", "mountstats", "network_route", "ntp", "pcidevice", "perf",
    "processes", "qdisc", "runit", "slabinfo", "softirqs", "supervisord", "swap", "sysctl",
    "systemd", "tcpstat", "wifi", "xfrm", "zoneinfo",
]
COLLECTORS_ALL = set(COLLECTORS_ENABLED) | set(COLLECTORS_DISABLED)

# A broad representative slice (enabled + disabled families) checked one-by-one for readable evidence.
COLLECTOR_SAMPLE = [
    "arp", "cpu", "meminfo", "loadavg", "diskstats", "filesystem", "netdev", "netstat", "stat",
    "uname", "os", "time", "hwmon", "thermal_zone", "nvme", "zfs", "xfs", "pressure",
    "buddyinfo", "cgroups", "ethtool", "interrupts", "ntp", "perf", "processes", "systemd",
    "tcpstat", "wifi", "sysctl", "slabinfo",
]

# General / web / path / log flags with their EXACT printed default (substring must appear verbatim).
FLAGS_WITH_DEFAULT = [
    '--web.listen-address=:9100',
    '--web.telemetry-path="/metrics"',
    '--web.config.file=""',
    '--web.max-requests=40',
    '--runtime.gomaxprocs=1',
    '--log.level=info',
    '--log.format=logfmt',
    '--path.procfs="/proc"',
    '--path.sysfs="/sys"',
    '--path.rootfs="/"',
    '--path.udev.data="/run/udev/data"',
]

# Boolean/global flags rendered as `--[no-]NAME`; the bare NAME must appear.
FLAGS_BOOL = [
    "web.disable-exporter-metrics", "web.systemd-socket", "collector.disable-defaults",
    "version", "help",
]

# Per-collector configuration flags with their EXACT documented default (proves real help reading).
CONFIG_WITH_DEFAULT = [
    '--collector.diskstats.device-exclude="^(z?ram|loop|fd|(h|s|v|xv)d[a-z]|nvme',
    '--collector.filesystem.mount-points-exclude="^/(dev|proc|run/credentials',
    '--collector.filesystem.fs-types-exclude="^(autofs|binfmt_misc|bpf|cgroup2?',
    '--collector.netstat.fields="^(.*_(InErrors|InErrs)',
    '--collector.ntp.server="127.0.0.1"',
    '--collector.ntp.server-port=123',
    '--collector.ntp.protocol-version=4',
    '--collector.ipvs.backend-labels="local_address,local_port,remote_address',
    '--collector.supervisord.url="http://localhost:9001/RPC2"',
    '--collector.vmstat.fields="^(oom_kill|pgpg|pswp',
    '--collector.runit.servicedir="/etc/service"',
    '--collector.slabinfo.slabs-include=".*"',
    '--collector.slabinfo.slabs-exclude=""',
    '--collector.ethtool.metrics-include=".*"',
    '--collector.netclass.ignored-devices="^$"',
    '--collector.powersupply.ignored-supplies="^$"',
    '--collector.tapestats.ignored-devices="^$"',
    '--collector.systemd.unit-include=".+"',
    '--collector.perf.cpus=""',
    '--collector.qdisc.fixtures=""',
    '--collector.wifi.fixtures=""',
]

# Per-collector configuration flags asserted by NAME (no default, or a value/regexp placeholder).
CONFIG_NAMES = [
    "--collector.arp.device-include", "--collector.arp.device-exclude",
    "--collector.cpu.info.flags-include", "--collector.cpu.info.bugs-include",
    "--collector.netdev.device-include", "--collector.netdev.device-exclude",
    "--collector.hwmon.chip-include", "--collector.hwmon.sensor-include",
    "--collector.textfile.directory", "--collector.sysctl.include",
    "--collector.sysctl.include-info", "--collector.pcidevice.idsfile",
    "--collector.qdisc.device-include",
]

# Boolean per-collector config toggles rendered as `--[no-]collector.X.Y`; the bare tail must appear.
CONFIG_BOOL = [
    "collector.arp.netlink", "collector.bcache.priorityStats", "collector.cpu.guest",
    "collector.cpu.info", "collector.interrupts.include-zeros", "collector.netclass.netlink",
    "collector.netclass_rtnl.with-stats", "collector.netdev.address-info", "collector.netdev.netlink",
    "collector.netdev.label-ifalias", "collector.pcidevice.names", "collector.rapl.enable-zone-label",
    "collector.stat.softirq", "collector.systemd.enable-task-metrics",
    "collector.systemd.enable-restarts-metrics", "collector.ntp.server-is-local",
]

# node_* metric families that the LIVE curated collector set exposes on-target. The first group is
# backed by /proc/stat, /proc/meminfo, /proc/loadavg, /proc/vmstat or pure syscalls; the second group
# is backed by the /proc/net/dev, /proc/diskstats and /proc/mounts (+statfs) sources.
LIVE_NODE_METRICS = [
    "node_cpu_seconds_total", "node_memory_MemTotal_bytes", "node_memory_MemFree_bytes",
    "node_load1", "node_boot_time_seconds", "node_context_switches_total", "node_intr_total",
    "node_time_seconds", "node_uname_info", "node_scrape_collector_success", "node_vmstat_pgfault",
    "node_network_receive_bytes_total", "node_network_transmit_bytes_total",
    "node_network_receive_packets_total", "node_disk_reads_completed_total",
    "node_disk_read_bytes_total", "node_disk_writes_completed_total",
    "node_filesystem_size_bytes", "node_filesystem_avail_bytes", "node_filesystem_files",
]


def check(cond, label):
    global _ok, _fail
    if cond:
        _ok += 1
    else:
        _fail += 1
        print("  FAIL %s" % label)


def run(args, timeout=60):
    try:
        r = subprocess.run(args, capture_output=True, text=True, timeout=timeout)
        return r.returncode, (r.stdout or "") + (r.stderr or "")
    except subprocess.TimeoutExpired:
        return 124, ""


def http_get(path, timeout=8):
    try:
        with _OPENER.open("http://%s:%d%s" % (HOST, PORT, path), timeout=timeout) as r:
            return r.getcode(), r.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode("utf-8", "replace")
    except Exception:
        return 0, ""


def metric_value(body, name, label_substr=None):
    """First sample value of Prometheus metric `name` whose line contains `label_substr`."""
    for line in body.splitlines():
        if not (line.startswith(name + "{") or line.startswith(name + " ")):
            continue
        if label_substr is not None and label_substr not in line:
            continue
        try:
            return float(line.rsplit(None, 1)[1])
        except (ValueError, IndexError):
            continue
    return None


def main():
    print("=== NodeExporterCarpet: node_exporter %s CLI + live /metrics on %s:%d ===" % (VER, HOST, PORT))
    check(os.path.exists(NODE_EXP), "binary present: %s" % NODE_EXP)
    if not os.path.exists(NODE_EXP):
        print("NE_RESULT ok=%d fail=%d" % (_ok, _fail))
        print("--- binary missing; cannot run ---")
        return 1

    # --- VER red-line: first line of --version ---
    rc, out = run([NODE_EXP, "--version"])
    first = out.splitlines()[0] if out.splitlines() else ""
    check(rc == 0 and re.match(r"^node_exporter, version %s " % re.escape(VER), first) is not None,
          "node_exporter --version first line == 'node_exporter, version %s ...': %r" % (VER, first[:64]))

    # --- HELP tree (exit 0, usage line, every general flag + exact default) ---
    rc, help_txt = run([NODE_EXP, "--help"])
    check(rc == 0, "node_exporter --help exits 0")
    check("usage: node_exporter [<flags>]" in help_txt, "--help prints 'usage: node_exporter [<flags>]'")
    for s in FLAGS_WITH_DEFAULT:
        check(s in help_txt, "--help documents %s" % s)
    for name in FLAGS_BOOL:
        check(("--[no-]%s" % name) in help_txt, "--help documents --[no-]%s" % name)

    # --- COLLECTOR enumeration: representative subset, exact 77-set equality, default classification ---
    for name in COLLECTOR_SAMPLE:
        check(("--[no-]collector.%s " % name) in help_txt, "--help enumerates --[no-]collector.%s" % name)
    # Parse the plain enable toggles: `--[no-]collector.<name>` where <name> has NO trailing dot
    # (config flags like --[no-]collector.arp.netlink are excluded by the whitespace lookahead).
    parsed = set(re.findall(r"--\[no-\]collector\.([a-z0-9_]+)(?=\s)", help_txt))
    check(len(parsed) == 77, "--help enumerates exactly 77 collector enable toggles (got %d)" % len(parsed))
    check(parsed == COLLECTORS_ALL,
          "parsed collector toggles == documented 77 (missing=%s extra=%s)"
          % (sorted(COLLECTORS_ALL - parsed), sorted(parsed - COLLECTORS_ALL)))
    # A few exact default-classification lines (single-line in help; wrapped ones are avoided).
    check("Enable the cpu collector (default: enabled)." in help_txt, "cpu collector default: enabled")
    check("Enable the ipvs collector (default: enabled)." in help_txt, "ipvs collector default: enabled")
    check("Enable the ntp collector (default: disabled)." in help_txt, "ntp collector default: disabled")
    check("Enable the swap collector (default: disabled)." in help_txt, "swap collector default: disabled")

    # --- per-collector configuration flags (defaults + names + bool toggles) ---
    for s in CONFIG_WITH_DEFAULT:
        check(s in help_txt, "--help documents %s" % s)
    for s in CONFIG_NAMES:
        check(s in help_txt, "--help documents %s" % s)
    for name in CONFIG_BOOL:
        check(("--[no-]%s" % name) in help_txt, "--help documents --[no-]%s" % name)

    # --- REJECT: bad enum values / invented flags are rejected (guards against fake-flag assertions) ---
    rc, out = run([NODE_EXP, "--log.level=BOGUS"])
    check(rc != 0 and "unrecognized log level" in out, "--log.level=BOGUS rejected (rc!=0 + message)")
    rc, out = run([NODE_EXP, "--log.format=BOGUS"])
    check(rc != 0 and "unrecognized log format" in out, "--log.format=BOGUS rejected (rc!=0 + message)")
    rc, out = run([NODE_EXP, "--collector.bogus_nonexistent"])
    check(rc != 0 and "unknown long flag" in out, "invented --collector flag rejected (rc!=0 + 'unknown long flag')")
    rc, out = run([NODE_EXP, "--this.flag.does.not.exist"])
    check(rc != 0 and "unknown long flag" in out, "invented global flag rejected (rc!=0 + 'unknown long flag')")

    # --- LIVE: headless exporter with /proc- and syscall-backed collectors only ---
    # These are the collectors whose data StarryOS genuinely provides: cpu/stat/meminfo/loadavg read
    # /proc files the kernel renders; vmstat reads /proc/vmstat (its default field filter matches
    # pgfault, so node_vmstat_pgfault surfaces); netdev reads /proc/net/dev (procfs mode -- the netlink
    # backend is disabled because StarryOS's rtnetlink does not carry per-link rtnl_link_stats64);
    # diskstats reads /proc/diskstats; filesystem reads /proc/self/mountinfo and statfs()es each mount;
    # uname/time are pure syscalls. Custom telemetry path + accepted log/path/limit flags (server
    # coming up == those flags accepted).
    log = open("/tmp/node_exporter_carpet.log", "w")
    proc = subprocess.Popen(
        [NODE_EXP,
         "--web.listen-address=%s:%d" % (HOST, PORT),
         "--web.telemetry-path=%s" % TELEMETRY,
         "--collector.disable-defaults",
         "--collector.cpu", "--collector.stat", "--collector.meminfo",
         "--collector.loadavg", "--collector.vmstat", "--collector.uname", "--collector.time",
         "--collector.netdev", "--no-collector.netdev.netlink",
         "--collector.diskstats", "--collector.filesystem",
         "--log.level=info", "--log.format=logfmt",
         "--web.max-requests=40", "--path.procfs=/proc"],
        stdout=log, stderr=subprocess.STDOUT)
    try:
        up = False
        body = ""
        for _ in range(90):
            if proc.poll() is not None:
                break
            code, body = http_get(TELEMETRY, timeout=4)
            if code == 200 and len(body) > 0:
                up = True
                break
            time.sleep(1)
        check(up, "exporter serving %s on %s:%d (custom --web.telemetry-path honored)" % (TELEMETRY, HOST, PORT))
        if not up:
            log.flush()
            try:
                print("--- node_exporter.log tail ---\n" + open("/tmp/node_exporter_carpet.log").read()[-1200:])
            except Exception:
                pass

        # /metrics must now be 404 -- proves the telemetry path really moved off the default.
        code404, _ = http_get("/metrics", timeout=6)
        check(code404 == 404, "default /metrics -> 404 after telemetry-path override (got %s)" % code404)

        # landing page served on / with the exporter's own title.
        codel, landing = http_get("/", timeout=6)
        check(codel == 200 and "Node Exporter" in landing, "GET / -> 200 landing page with 'Node Exporter' (got %s)" % codel)

        # build_info exposes the pinned version as a label (version collector, always registered).
        check(re.search(r'node_exporter_build_info\{[^}]*version="%s"[^}]*\}\s+1' % re.escape(VER), body) is not None,
              "node_exporter_build_info exposes version=%s" % VER)

        # the documented node_* families the curated collectors produce.
        for m in LIVE_NODE_METRICS:
            check(re.search(r"^%s(\{|\s)" % re.escape(m), body, re.M) is not None, "/metrics exposes %s" % m)

        # --- procfs-backed collectors carry REAL, non-zero data (not just an exposed-but-empty family) ---
        # netdev: the scrape itself flows over loopback, so lo has received bytes by collection time.
        lo_rx = metric_value(body, "node_network_receive_bytes_total", 'device="lo"')
        check(lo_rx is not None and lo_rx > 0,
              'node_network_receive_bytes_total{device="lo"} > 0 from /proc/net/dev (got %r)' % lo_rx)
        # diskstats: the root virtio-blk device served boot reads, so vda read counters are non-zero.
        vda_reads = metric_value(body, "node_disk_reads_completed_total", 'device="vda"')
        check(vda_reads is not None and vda_reads > 0,
              'node_disk_reads_completed_total{device="vda"} > 0 from /proc/diskstats (got %r)' % vda_reads)
        vda_rbytes = metric_value(body, "node_disk_read_bytes_total", 'device="vda"')
        check(vda_rbytes is not None and vda_rbytes > 0,
              'node_disk_read_bytes_total{device="vda"} > 0 from /proc/diskstats (got %r)' % vda_rbytes)
        # vmstat: /proc/vmstat's pgfault counter -- demand paging has serviced faults by scrape time.
        pgfault = metric_value(body, "node_vmstat_pgfault")
        check(pgfault is not None and pgfault > 0,
              "node_vmstat_pgfault > 0 from /proc/vmstat (got %r)" % pgfault)
        # filesystem: statfs() on the ext4 root returns a real total size.
        root_size = metric_value(body, "node_filesystem_size_bytes", 'mountpoint="/"')
        check(root_size is not None and root_size > 0,
              'node_filesystem_size_bytes{mountpoint="/"} > 0 from /proc/mounts + statfs (got %r)' % root_size)
        root_avail = metric_value(body, "node_filesystem_avail_bytes", 'mountpoint="/"')
        check(root_avail is not None and root_avail >= 0,
              'node_filesystem_avail_bytes{mountpoint="/"} present from statfs (got %r)' % root_avail)

        # Go-runtime exporter metric (present unless --web.disable-exporter-metrics; we do NOT set it).
        check(re.search(r"^go_goroutines(\{|\s)", body, re.M) is not None, "/metrics exposes go_goroutines")

        # Prometheus exposition format sanity: node_ HELP + TYPE header lines present.
        check("# HELP node_" in body, "/metrics has '# HELP node_' exposition headers")
        check("# TYPE node_" in body, "/metrics has '# TYPE node_' exposition headers")
    finally:
        try:
            proc.send_signal(signal.SIGTERM)
            proc.wait(timeout=8)
        except Exception:
            try:
                proc.kill()
            except Exception:
                pass
        log.close()

    print("NE_RESULT ok=%d fail=%d" % (_ok, _fail))
    if _fail == 0:
        print("NE_DONE")
        return 0
    return 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as e:
        import traceback
        traceback.print_exc()
        print("NE_RESULT ok=%d fail=%d" % (_ok, _fail + 1))
        sys.exit(1)
