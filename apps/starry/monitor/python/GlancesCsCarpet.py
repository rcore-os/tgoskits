#!/usr/bin/env python3
# GlancesCsCarpet.py -- glances CLIENT/SERVER mode. Starts a glances XML-RPC server (`glances -s`)
# on loopback, waits until the server is actually answering XML-RPC with real stats, then runs a
# glances client (`glances -c`) that connects to it and pulls a one-shot snapshot over the wire.
# Asserts the client received a plausible mem.total FROM the server -> proves the XML-RPC server
# (bind/listen/accept) + client (connect) round-trip on StarryOS loopback.
#
# Readiness matters: glances prints "XML-RPC server is running" from GlancesServer.__init__, but the
# instance it then registers runs a full initial stats.update() *before* main enters the accept
# loop. On a slow emulated target that gap can exceed the client's 7s XML-RPC transport timeout, so
# a client that connects the instant the banner appears times out and silently falls back to SNMP
# (glances.client._login_glances), yielding an empty snapshot. We therefore gate the client on a
# carpet-side XML-RPC probe (same xmlrpc.client transport the glances client uses) that confirms the
# server actually serves a real mem.total before the client is launched.
#
# Emits `GCS_RESULT ok=<N> fail=<F>` and, only when F==0, `GCS_DONE`.
import json, os, re, signal, socket, subprocess, sys, time
import xmlrpc.client

GLANCES = os.environ.get("GLANCES_BIN", "glances")
DISABLE = "ip,cloud,containers,docker,folders,ports,smart,wifi,gpu,connections,sensors"
PORT = int(os.environ.get("MONITOR_CS_PORT", "61234"))
HOST = "127.0.0.1"
SRV_LOG = "/tmp/glances-server.log"
CLI_LOG = "/tmp/glances-client.log"
_ok = 0
_fail = 0


def check(cond, label):
    global _ok, _fail
    if cond:
        _ok += 1
    else:
        _fail += 1
        print("  FAIL %s" % label)


def read_file(path):
    try:
        with open(path) as f:
            return f.read()
    except Exception:
        return ""


def dump_logs(where):
    for tag, path in (("srv", SRV_LOG), ("cli", CLI_LOG)):
        lines = read_file(path).splitlines()
        print("  --- %s %s log (last 30 lines) ---" % (where, tag))
        for ln in lines[-30:]:
            print("  %s| %s" % (tag, ln))


def server_reports_ready(server):
    if server.poll() is not None:
        return False
    txt = read_file(SRV_LOG)
    return "XML-RPC server is running" in txt or ("running on %s:%d" % (HOST, PORT)) in txt


def probe_mem_total(server, deadline):
    """Poll the server over XML-RPC until getAll() yields a real mem.total (bytes), or the deadline
    passes / the server dies. Uses the same xmlrpc.client transport the glances client uses, so a
    success here proves the C/S round-trip works at the protocol level on StarryOS loopback."""
    socket.setdefaulttimeout(15)
    try:
        while time.time() < deadline:
            if server.poll() is not None:
                return None
            try:
                proxy = xmlrpc.client.ServerProxy("http://%s:%d" % (HOST, PORT))
                proxy.init()
                data = json.loads(proxy.getAll())
                total = int(data.get("mem", {}).get("total", 0)) if isinstance(data, dict) else 0
                if total > (1 << 20):
                    return total
            except Exception:
                pass
            time.sleep(1)
    finally:
        socket.setdefaulttimeout(None)
    return None


def main():
    print("=== GlancesCsCarpet: glances -s (XML-RPC server) + glances -c (client) on :%d ===" % PORT)
    slog = open(SRV_LOG, "w")
    server = subprocess.Popen(
        [GLANCES, "-s", "-p", str(PORT), "-B", HOST, "--disable-autodiscover", "-t", "2",
         "--disable-check-update", "--disable-plugin", DISABLE],
        stdout=slog, stderr=subprocess.STDOUT)
    client = None
    try:
        # 1. wait for the server to announce it is listening.
        up = False
        for _ in range(90):
            if server_reports_ready(server):
                up = True
                break
            if server.poll() is not None:
                break
            time.sleep(1)
        check(up, "glances -s XML-RPC server came up (announced listening)")

        # 2. gate on a real XML-RPC round-trip: prove the server serves a plausible mem.total before
        #    the client connects (absorbs the slow initial-update window on emulated targets).
        probe_total = probe_mem_total(server, time.time() + 120) if up else None
        check(probe_total is not None,
              "server answers XML-RPC getAll() with a real mem.total (probe got %s)" % probe_total)

        # 3. run the real glances client bounded: it fetches from the server and prints
        #    mem.total/cpu.total. Give it a few refresh cycles so a transient miss retries.
        clog = open(CLI_LOG, "w")
        client = subprocess.Popen(
            [GLANCES, "-c", HOST, "-p", str(PORT), "--stdout", "mem.total,cpu.total",
             "--stop-after", "4", "-t", "2", "--disable-check-update"],
            stdout=clog, stderr=subprocess.STDOUT)
        got = None
        deadline = time.time() + 60
        while time.time() < deadline:
            txt = read_file(CLI_LOG)
            m = re.search(r"^mem\.total:\s*([0-9]+)\s*$", txt, re.M)
            if m:
                got = int(m.group(1))
                break
            if client.poll() is not None:
                m = re.search(r"^mem\.total:\s*([0-9]+)\s*$", read_file(CLI_LOG), re.M)
                if m:
                    got = int(m.group(1))
                break
            time.sleep(1)
        clog.close()
        check(got is not None, "client received a mem.total line from the server")
        check(got is not None and got > (1 << 20),
              "client mem.total > 1 MiB over the wire (got %s) -- c/s round-trip real" % got)
        c = re.search(r"^cpu\.total:\s*([0-9.]+)\s*$", read_file(CLI_LOG), re.M)
        check(c is not None, "client also received cpu.total from the server")

        if _fail:
            dump_logs("failure")
    finally:
        for p in (client, server):
            if p is None:
                continue
            try:
                p.send_signal(signal.SIGTERM)
                p.wait(timeout=6)
            except Exception:
                try:
                    p.kill()
                except Exception:
                    pass
        slog.close()

    print("GCS_RESULT ok=%d fail=%d" % (_ok, _fail))
    if _fail == 0:
        print("GCS_DONE")
        return 0
    return 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as e:
        import traceback
        traceback.print_exc()
        print("GCS_RESULT ok=%d fail=%d" % (_ok, _fail + 1))
        sys.exit(1)
