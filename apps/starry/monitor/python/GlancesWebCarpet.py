#!/usr/bin/env python3
# GlancesWebCarpet.py -- glances WEB server mode (`glances -w`): a FastAPI app served by uvicorn on
# loopback. Drives the REAL HTTP REST API (Starlette/uvicorn ASGI + the Go-free python net stack on
# StarryOS loopback) and asserts JSON round-trips: /api/4/status, /api/4/all (full snapshot),
# /api/4/mem/total (a plausible RAM size), /api/4/cpu (canonical fields).
#
# Emits `GWEB_RESULT ok=<N> fail=<F>` and, only when F==0, `GWEB_DONE`.
import json, os, signal, subprocess, sys, time, urllib.request, urllib.error

GLANCES = os.environ.get("GLANCES_BIN", "glances")
DISABLE = "ip,cloud,containers,docker,folders,ports,smart,wifi,gpu,connections,sensors"
PORT = int(os.environ.get("MONITOR_WEB_PORT", "62080"))
HOST = "127.0.0.1"
_ok = 0
_fail = 0

# ignore any inherited HTTP(S)_PROXY env: loopback must be reached directly.
_OPENER = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def check(cond, label):
    global _ok, _fail
    if cond:
        _ok += 1
    else:
        _fail += 1
        print("  FAIL %s" % label)


def get(path, timeout=8):
    req = urllib.request.Request("http://%s:%d%s" % (HOST, PORT, path))
    try:
        with _OPENER.open(req, timeout=timeout) as r:
            return r.getcode(), r.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()
    except Exception:
        return 0, b""


def main():
    print("=== GlancesWebCarpet: glances -w (FastAPI/uvicorn) REST API on :%d ===" % PORT)
    logf = open("/tmp/glances-web.log", "w")
    proc = subprocess.Popen(
        [GLANCES, "-w", "-p", str(PORT), "--disable-webui", "-t", "2",
         "--disable-check-update", "--disable-plugin", DISABLE],
        stdout=logf, stderr=subprocess.STDOUT)
    try:
        # wait for uvicorn to serve /api/4/status (Go-free ASGI cold start can lag under TCG).
        up = False
        for _ in range(120):
            if proc.poll() is not None:
                break
            code, _b = get("/api/4/status", timeout=4)
            if code == 200:
                up = True
                break
            time.sleep(1)
        check(up, "glances -w uvicorn served /api/4/status == 200")
        if not up:
            logf.flush()
            try:
                print("--- glances-web.log tail ---\n" + open("/tmp/glances-web.log").read()[-1200:])
            except Exception:
                pass
        else:
            # /api/4/all -- the full stats snapshot as one JSON document.
            code, body = get("/api/4/all", timeout=12)
            check(code == 200 and len(body) > 500, "/api/4/all == 200 with a substantial JSON body (%d B)" % len(body))
            try:
                allobj = json.loads(body)
                check(isinstance(allobj, dict) and "cpu" in allobj and "mem" in allobj,
                      "/api/4/all JSON has cpu + mem sections")
            except Exception as e:
                check(False, "/api/4/all parses as JSON (%r)" % e)

            # /api/4/mem/total -- plausible RAM size via the REST path.
            code, body = get("/api/4/mem/total", timeout=8)
            check(code == 200, "/api/4/mem/total == 200")
            try:
                obj = json.loads(body)
                check(int(obj.get("total", 0)) > (1 << 20), "/api/4/mem/total total > 1 MiB (got %s)" % obj)
            except Exception as e:
                check(False, "/api/4/mem/total parses as JSON (%r)" % e)

            # /api/4/cpu -- canonical cpu fields via REST.
            code, body = get("/api/4/cpu", timeout=8)
            check(code == 200, "/api/4/cpu == 200")
            try:
                obj = json.loads(body)
                check(isinstance(obj, dict) and "total" in obj and "system" in obj,
                      "/api/4/cpu JSON has total + system")
            except Exception as e:
                check(False, "/api/4/cpu parses as JSON (%r)" % e)
    finally:
        try:
            proc.send_signal(signal.SIGTERM)
            proc.wait(timeout=8)
        except Exception:
            try:
                proc.kill()
            except Exception:
                pass
        logf.close()

    print("GWEB_RESULT ok=%d fail=%d" % (_ok, _fail))
    if _fail == 0:
        print("GWEB_DONE")
        return 0
    return 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as e:
        import traceback
        traceback.print_exc()
        print("GWEB_RESULT ok=%d fail=%d" % (_ok, _fail + 1))
        sys.exit(1)
