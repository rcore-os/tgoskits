#!/usr/bin/env python3
# GlancesCsCarpet.py -- glances CLIENT/SERVER mode. Starts a glances XML-RPC server (`glances -s`)
# on loopback, then runs a glances client (`glances -c`) that connects to it and pulls a one-shot
# snapshot over the wire. Asserts the client received a plausible mem.total FROM the server ->
# proves the XML-RPC server (bind/listen/accept) + client (connect) round-trip on StarryOS loopback.
#
# Emits `GCS_RESULT ok=<N> fail=<F>` and, only when F==0, `GCS_DONE`.
import os, re, signal, subprocess, sys, time

GLANCES = os.environ.get("GLANCES_BIN", "glances")
DISABLE = "ip,cloud,containers,docker,folders,ports,smart,wifi,gpu,connections,sensors"
PORT = int(os.environ.get("MONITOR_CS_PORT", "61234"))
HOST = "127.0.0.1"
_ok = 0
_fail = 0


def check(cond, label):
    global _ok, _fail
    if cond:
        _ok += 1
    else:
        _fail += 1
        print("  FAIL %s" % label)


def main():
    print("=== GlancesCsCarpet: glances -s (XML-RPC server) + glances -c (client) on :%d ===" % PORT)
    slog = open("/tmp/glances-server.log", "w")
    server = subprocess.Popen(
        [GLANCES, "-s", "-p", str(PORT), "-B", HOST, "--disable-autodiscover", "-t", "2",
         "--disable-check-update", "--disable-plugin", DISABLE],
        stdout=slog, stderr=subprocess.STDOUT)
    client = None
    try:
        # wait for the server to announce it is listening.
        up = False
        for _ in range(90):
            if server.poll() is not None:
                break
            try:
                txt = open("/tmp/glances-server.log").read()
            except Exception:
                txt = ""
            if "XML-RPC server is running" in txt or ("running on %s:%d" % (HOST, PORT)) in txt:
                up = True
                break
            time.sleep(1)
        check(up, "glances -s XML-RPC server came up (announced listening)")

        # run the client bounded: it fetches from the server and prints mem.total/cpu.total. In
        # client stdout mode --stop-after is not always honoured, so bound it with a timeout and
        # read whatever it printed to the log.
        clog_path = "/tmp/glances-client.log"
        clog = open(clog_path, "w")
        client = subprocess.Popen(
            [GLANCES, "-c", HOST, "-p", str(PORT), "--stdout", "mem.total,cpu.total",
             "--stop-after", "1", "-t", "2", "--disable-check-update"],
            stdout=clog, stderr=subprocess.STDOUT)
        got = None
        deadline = time.time() + 40
        while time.time() < deadline:
            try:
                txt = open(clog_path).read()
            except Exception:
                txt = ""
            m = re.search(r"^mem\.total:\s*([0-9]+)\s*$", txt, re.M)
            if m:
                got = int(m.group(1))
                break
            if client.poll() is not None:
                # process finished; re-read once more
                try:
                    txt = open(clog_path).read()
                except Exception:
                    pass
                m = re.search(r"^mem\.total:\s*([0-9]+)\s*$", txt, re.M)
                if m:
                    got = int(m.group(1))
                break
            time.sleep(1)
        clog.close()
        check(got is not None, "client received a mem.total line from the server")
        check(got is not None and got > (1 << 20),
              "client mem.total > 1 MiB over the wire (got %s) -- c/s round-trip real" % got)
        c = None
        try:
            c = re.search(r"^cpu\.total:\s*([0-9.]+)\s*$", open(clog_path).read(), re.M)
        except Exception:
            pass
        check(c is not None, "client also received cpu.total from the server")
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
