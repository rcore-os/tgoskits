#!/usr/bin/env python3
# GrafanaCarpet.py -- headless carpet for Grafana (the monitoring/observability web app). Grafana is
# a single fully-static CGO-free Go binary (`grafana server` subcommand) that serves its embedded
# frontend SPA + REST API on loopback :3000 backed by an embedded SQLite store -- so, like glances
# -w and prometheus, it is exercised HEADLESS via HTTP assertions (no browser, no TUI).
#
# DIMENSIONS (gated PASS/TOTAL -- no silent pass):
#   VER    -- `grafana --version` reports EXACTLY 13.0.1 (red-line).
#   LISTEN -- `grafana server` reaches "HTTP Server Listen" and answers /api/health (server up +
#             SQLite migrations done). Uses a fresh writable data dir seeded from the prebuild's
#             pre-migrated arch-independent grafana.db when present (skips the 709 first-run
#             migrations); otherwise grafana migrates on the spot (exercises the StarryOS ext4/fsync
#             path -- legitimate, just slower).
#   HEALTH -- GET /api/health -> 200 + JSON {"database":"ok","version":"13.0.1"} (SQLite backend up).
#   FRONTEND -- GET /login -> 200 serving the embedded Grafana SPA (title "Grafana" + bootdata).
#   API    -- GET / (root) reaches the app (200/30x to /login), and /robots.txt is served (static
#             asset path from the embedded public/ tree).
#
# Exercises the Go runtime, loopback HTTP, and the embedded SQLite store on the ext4 image.
# Emits `GRAF_RESULT ok=<N> fail=<F>` and, only when F==0, `GRAF_DONE`.
import json, os, re, shutil, signal, subprocess, sys, tempfile, time, urllib.request, urllib.error

GRAFANA = os.environ.get("GRAFANA_BIN", "/opt/grafana/bin/grafana")
HOMEPATH = os.environ.get("GRAFANA_HOMEPATH", "/opt/grafana")
PORT = int(os.environ.get("MONITOR_GRAFANA_PORT", "3000"))
HOST = "127.0.0.1"
VER = "13.0.1"
TRIES = int(os.environ.get("MONITOR_GRAFANA_HEALTH_TRIES", "1200"))  # 1s each; big budget for TCG migration
_ok = 0
_fail = 0
_OPENER = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def check(cond, label):
    global _ok, _fail
    if cond:
        _ok += 1
    else:
        _fail += 1
        print("  FAIL %s" % label)


def http(path, timeout=8):
    req = urllib.request.Request("http://%s:%d%s" % (HOST, PORT, path))
    try:
        with _OPENER.open(req, timeout=timeout) as r:
            return r.getcode(), r.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()
    except Exception:
        return 0, b""


def run(args, timeout=90):
    try:
        r = subprocess.run(args, capture_output=True, text=True, timeout=timeout)
        return r.returncode, (r.stdout or "") + (r.stderr or "")
    except subprocess.TimeoutExpired:
        return 124, ""


def carpet_help(path, needles, timeout=90):
    # `grafana <path...> --help` -- urfave/cli prints this node's help and exits 0. Pure
    # introspection: no config, no db, no network, no server spun up. Proves the command/subcommand
    # actually EXISTS + invokable, then asserts its documented flag/subcommand surface. The
    # --help/flag surface is arch-independent, so the x86 ground truth holds on every target arch
    # under musl (the overlay binary is the target-arch build of the identical CLI tree).
    label = "grafana " + " ".join(path) if path else "grafana"
    rc, out = run([GRAFANA] + list(path) + ["--help"], timeout=timeout)
    check(rc == 0, "`%s --help` exits 0" % label)
    for n in needles:
        check(n in out, "`%s --help` documents %r" % (label, n))


def main():
    print("=== GrafanaCarpet: headless grafana %s server + HTTP/API assertions on :%d ===" % (VER, PORT))
    check(os.path.exists(GRAFANA), "grafana binary present: %s" % GRAFANA)
    if not os.path.exists(GRAFANA):
        print("GRAF_RESULT ok=%d fail=%d" % (_ok, _fail))
        return 1

    # VER red-line.
    r = subprocess.run([GRAFANA, "--version"], capture_output=True, text=True, timeout=60)
    out = (r.stdout or "") + (r.stderr or "")
    check(r.returncode == 0 and re.search(r"grafana version %s\b" % re.escape(VER), out) is not None,
          "grafana --version == %s (red-line): %r" % (VER, out.strip()[:60]))
    # short-form version flag (the top-level `-v` mirrors `--version`).
    rc, vout = run([GRAFANA, "-v"])
    check(rc == 0 and ("grafana version %s" % VER) in vout, "grafana -v == grafana version %s" % VER)

    # ---- CLI HELP TREE (grafana 13.0.1) ----------------------------------------------------------
    # Carpet-level coverage of the ENTIRE grafana CLI surface: every command x subcommand x
    # documented flag, ground-truthed against the binary's OWN `--help` at each node (depth reaches 4
    # at `grafana cli admin secrets-migration re-encrypt`). Only flags/subcommands that real --help
    # actually prints are asserted -- never invented. All nodes are pure `--help` introspection that
    # exits 0 without a config/db/network/server, so the whole tree is deterministic + bounded.

    # top level: the two real commands (cli, server) + built-in help + the global options. No
    # standalone grafana-cli/grafana-server binaries exist; the legacy names are only deprecated
    # bash wrappers that exec `grafana cli|server`, so `grafana cli`/`grafana server` IS that surface.
    carpet_help([], ["cli", "server", "help, h", "--help, -h", "--version, -v",
                     "Grafana server and command line interface"])

    # `grafana cli`: the plugin flags (--pluginsDir/--repo/--pluginUrl/--insecure/--debug) and their
    # env-var hints live on THIS level, not on the individual plugins/admin leaves.
    carpet_help(["cli"], ["plugins", "admin", "help, h", "--config", "--homepath",
                          "--configOverrides", "--version, -v", "--debug", "--pluginsDir", "--repo",
                          "--pluginUrl", "--insecure", "--help, -h",
                          "$GF_PLUGIN_DIR", "$GF_PLUGIN_REPO", "$GF_PLUGIN_URL"])

    # `grafana cli plugins`: the alias renderings (update, upgrade / update-all, upgrade-all /
    # uninstall, remove) are part of the real --help and are asserted verbatim.
    carpet_help(["cli", "plugins"],
                ["install", "list-remote", "list-versions", "update, upgrade",
                 "update-all, upgrade-all", "ls", "uninstall, remove", "help, h", "--help, -h"])
    # plugins leaves: install/list-remote/list-versions/update/update-all/ls/uninstall each reach out
    # to grafana.com/api/plugins (or a plugin-zip URL) to do real work -- a live network path that is
    # out of scope for a headless on-target carpet. We assert each leaf's --help (proving the
    # subcommand exists via its NAME line + its --help,-h flag surface); the real args are positional
    # (e.g. `install <plugin id> <version>`), so --help,-h is the entire documented flag set.
    for leaf in ("install", "list-remote", "list-versions", "update", "update-all", "ls",
                 "uninstall"):
        carpet_help(["cli", "plugins", leaf], ["cli plugins " + leaf, "--help, -h"])

    # `grafana cli admin`: reset-admin-password / data-migration / secrets-migration /
    # secrets-consolidation / flush-rbac-seed-assignment each spin up the FULL sqlstore and mutate a
    # migrated grafana.db. On-target under TCG that path is unbounded (the 700+ migration chain) and
    # mutates DB state, so we assert their --help surface here; the live migrate+serve DB path is
    # exercised instead by the headless `grafana server` run below.
    carpet_help(["cli", "admin"],
                ["reset-admin-password", "data-migration", "secrets-migration",
                 "secrets-consolidation", "flush-rbac-seed-assignment", "help, h", "--help, -h"])
    carpet_help(["cli", "admin", "reset-admin-password"],
                ["--password-from-stdin", "--user-id", "--help, -h"])
    carpet_help(["cli", "admin", "data-migration"],
                ["encrypt-datasource-passwords", "help, h", "--help, -h"])
    carpet_help(["cli", "admin", "data-migration", "encrypt-datasource-passwords"],
                ["data-migration encrypt-datasource-passwords", "--help, -h"])
    carpet_help(["cli", "admin", "secrets-migration"],
                ["re-encrypt", "rollback", "re-encrypt-data-keys", "help, h", "--help, -h"])
    carpet_help(["cli", "admin", "secrets-migration", "re-encrypt"],
                ["secrets-migration re-encrypt", "--help, -h"])
    carpet_help(["cli", "admin", "secrets-migration", "rollback"],
                ["secrets-migration rollback", "--help, -h"])
    carpet_help(["cli", "admin", "secrets-migration", "re-encrypt-data-keys"],
                ["secrets-migration re-encrypt-data-keys", "--help, -h"])
    carpet_help(["cli", "admin", "secrets-consolidation"],
                ["consolidate", "help, h", "--help, -h"])
    # the one admin leaf with a rich flag set.
    carpet_help(["cli", "admin", "secrets-consolidation", "consolidate"],
                ["--config", "--homepath", "--configOverrides", "--chunk-size", "--threads",
                 "--benchmark", "--cpuprofile", "--memprofile", "--cpu-profile-rate", "--help, -h"])
    carpet_help(["cli", "admin", "flush-rbac-seed-assignment"],
                ["flush-rbac-seed-assignment", "--help, -h"])

    # `grafana server` full flag set (the actual server run below drives the functional path). Its
    # `target` subcommand mirrors the identical flag set for targeted-service selection.
    server_flags = ["--config", "--homepath", "--pidfile", "--packaging", "--configOverrides",
                    "--version, -v", "--vv", "--profile", "--profile-addr", "--profile-port",
                    "--profile-block-rate", "--profile-mutex-rate", "--tracing", "--tracing-file",
                    "--help, -h"]
    carpet_help(["server"], ["target"] + server_flags)
    carpet_help(["server", "target"], server_flags)

    # fresh, writable, isolated data dir seeded from the pre-migrated db when present.
    workdir = tempfile.mkdtemp(prefix="grafana-run-")
    data = os.path.join(workdir, "data")
    logs = os.path.join(workdir, "logs")
    plugins = os.path.join(workdir, "plugins")
    prov = os.path.join(workdir, "provisioning")
    for d in (data, logs, plugins,
              os.path.join(prov, "datasources"), os.path.join(prov, "dashboards"),
              os.path.join(prov, "plugins"), os.path.join(prov, "alerting"),
              os.path.join(prov, "access-control"), os.path.join(prov, "notifiers")):
        os.makedirs(d, exist_ok=True)
    seed = os.path.join(HOMEPATH, "data", "grafana.db")
    if os.path.isfile(seed):
        shutil.copy2(seed, os.path.join(data, "grafana.db"))
        print("  seeded pre-migrated grafana.db (migrations will be skipped)")
    else:
        print("  no pre-migrated grafana.db -> grafana will migrate on the spot (slower)")

    ini = os.path.join(workdir, "grafana.ini")
    with open(ini, "w") as f:
        f.write(
            "app_mode = production\n"
            "[paths]\ndata = %s\nlogs = %s\nplugins = %s\nprovisioning = %s\n"
            "[server]\nhttp_addr = %s\nhttp_port = %d\n"
            # wal = true: grafana 13's unified-storage apiserver and the sqlstore both touch
            # grafana.db during startup; WAL (multi-reader + single-writer) cuts the transient
            # SQLITE_BUSY contention so the frontend endpoints come up cleanly. level=info so the
            # "HTTP Server Listen" / "migrations completed" log lines are emitted for assertion.
            "[database]\ntype = sqlite3\nwal = true\n"
            "[analytics]\nreporting_enabled = false\ncheck_for_updates = false\ncheck_for_plugin_updates = false\n"
            "[log]\nmode = console\nlevel = info\n"
            % (data, logs, plugins, prov, HOST, PORT))

    logpath = os.path.join(workdir, "server.log")
    logf = open(logpath, "w")
    proc = subprocess.Popen([GRAFANA, "server", "--homepath", HOMEPATH, "--config", ini],
                            stdout=logf, stderr=subprocess.STDOUT)
    try:
        up = False
        for _ in range(TRIES):
            if proc.poll() is not None:
                break
            code, _b = http("/api/health", timeout=4)
            if code == 200:
                up = True
                break
            time.sleep(1)
        logf.flush()
        try:
            log = open(logpath).read()
        except Exception:
            log = ""
        check(up, "grafana server reached /api/health == 200")
        check("HTTP Server Listen" in log, "server log emitted 'HTTP Server Listen'")
        check("migrations completed" in log, "server log emitted 'migrations completed' (SQLite store migrated)")
        if not up:
            print("--- server.log tail ---\n" + log[-1500:])
        else:
            # HEALTH JSON: database ok + version red-line.
            code, body = http("/api/health", timeout=8)
            try:
                h = json.loads(body)
                check(code == 200 and h.get("database") == "ok", "/api/health database == ok: %s" % h)
                check(h.get("version") == VER, "/api/health version == %s (red-line): %s" % (VER, h.get("version")))
            except Exception as e:
                check(False, "/api/health parses as JSON (%r): %r" % (e, body[:80]))

            # FRONTEND: embedded SPA served on /login. The unified-storage apiserver keeps grafana.db
            # transiently busy right after /api/health flips green, so retry a few times until the
            # SPA route settles (this is grafana startup churn, not a real failure).
            code, txt = 0, ""
            for _ in range(30):
                code, body = http("/login", timeout=10)
                txt = body.decode("utf-8", "replace")
                if code == 200:
                    break
                time.sleep(2)
            check(code == 200, "/login == 200 (embedded frontend served)")
            check("Grafana" in txt and ("grafanaBootData" in txt or "<title>Grafana</title>" in txt),
                  "/login serves the embedded Grafana SPA (title/bootdata)")

            # API/static: root reaches the app; robots.txt served from embedded public/.
            code, _b = http("/", timeout=8)
            check(code in (200, 301, 302), "GET / reaches the app (code %s)" % code)
            rc = 0
            for _ in range(15):
                rc, _b = http("/robots.txt", timeout=8)
                if rc == 200:
                    break
                time.sleep(2)
            check(rc == 200, "/robots.txt static asset served (code %s)" % rc)
    finally:
        try:
            proc.send_signal(signal.SIGTERM)
            proc.wait(timeout=15)
        except Exception:
            try:
                proc.kill()
            except Exception:
                pass
        logf.close()
        shutil.rmtree(workdir, ignore_errors=True)

    print("GRAF_RESULT ok=%d fail=%d" % (_ok, _fail))
    if _fail == 0:
        print("GRAF_DONE")
        return 0
    return 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as e:
        import traceback
        traceback.print_exc()
        print("GRAF_RESULT ok=%d fail=%d" % (_ok, _fail + 1))
        sys.exit(1)
