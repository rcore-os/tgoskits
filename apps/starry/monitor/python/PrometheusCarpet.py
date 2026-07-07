#!/usr/bin/env python3
# PrometheusCarpet.py -- industrial end-to-end carpet for the Prometheus monitoring stack on
# StarryOS. prometheus + promtool are fully-static, CGO-disabled Go binaries (netgo,builtinassets =>
# embedded web UI, no libc/ld-musl wiring). node_exporter 1.11.1 (the simplest exporter, also a
# static Go binary) is run as a REAL scrape target so the CORE scrape->ingest->query pipeline is
# exercised, not just a synthetic scalar.
#
# DIMENSIONS (all real, gated PASS/TOTAL -- no silent pass):
#   VER      -- prometheus --version AND promtool --version report EXACTLY 3.11.3 (red-line;
#               a wrong version = invalid test); node_exporter --version == 1.11.1.
#   HELP     -- CARPET-LEVEL CLI surface. Ground truth = the tools' OWN --help. Every prometheus
#               server flag is asserted in --help-long; the promtool root --help lists every one of
#               its 27 leaf commands (exact usage signature) plus the persistent global flags; and
#               every leaf's own `<cmd> --help` is spawned to assert its usage line, the four
#               persistent global flags, and its documented group-persistent + leaf-specific flag
#               surface. No flag is invented -- each substring is one the real --help prints.
#   FUNC     -- deterministic, server-FREE promtool behaviors: check config (good+bad), check rules
#               (good+bad), check metrics lint (good+bad, over stdin), check web-config (good+bad),
#               promql format + label-matchers set/delete (the --experimental PromQL rewriters),
#               test rules (a rule unit-test driving the PromQL engine), and the TSDB import path
#               (create-blocks-from openmetrics -> tsdb list -> tsdb analyze, which exercises block
#               writing + WAL replay on the ext4 image).
#   READY    -- headless prometheus server on loopback :9090 logs "Server is ready to receive web
#               requests." AND /-/ready answers Ready (HTTP serving + TSDB open).
#   QUERY    -- /api/v1/query?query=vector(42) -> status success + value "42" (PromQL engine), and
#               promtool query instant/labels/series driven as a live PromQL client.
#   SCRAPE   -- /api/v1/query?query=up{job="node"} -> value "1" (node_exporter really scraped +
#               ingested through loopback :9090 -> :9100).
#
# Live-server-only promtool leaves (check healthy/ready, query range/analyze, debug pprof/metrics/all,
# push metrics, tsdb bench write, tsdb create-blocks-from rules) are covered at the --help/usage
# level: driving them needs a second running server, a remote-write receiver, the multi-MB
# 20kseries.json fixture, or histogram-typed data that the on-target scrape doesn't produce, so their
# existence + flag surface is the in-scope assertion (the --help surface is arch-independent).
#
# Exercises the Go runtime (M:N scheduler, GC, futex parking, getrandom), the TSDB head/WAL on the
# ext4 image, and the loopback net stack (bind/listen/accept + the scrape client). Emits
# `PROM_RESULT ok=<N> fail=<F>` and, only when F==0, `PROM_DONE`.
import os, re, shutil, signal, subprocess, sys, tempfile, time, urllib.request, urllib.error

PROM = os.environ.get("PROMETHEUS_BIN", "/usr/local/bin/prometheus")
PROMTOOL = os.environ.get("PROMTOOL_BIN", "/usr/local/bin/promtool")
NODE_EXP = os.environ.get("NODE_EXPORTER_BIN", "/usr/bin/node_exporter")
CFG = os.environ.get("PROMETHEUS_CFG", "/etc/prometheus.yml")
TSDB = os.environ.get("PROMETHEUS_TSDB", "/root/prom")
EP = os.environ.get("PROMETHEUS_EP", "127.0.0.1:9090")
NE_EP = os.environ.get("NODE_EXPORTER_EP", "127.0.0.1:9100")
VER = "3.11.3"
NE_VER = "1.11.1"
_ok = 0
_fail = 0
_OPENER = urllib.request.build_opener(urllib.request.ProxyHandler({}))


# Persistent global flags kingpin merges into EVERY promtool command (root -> group -> leaf); the
# ground truth lists them as present on every leaf, so the per-leaf loop asserts all four persist.
GLOBALS = ["-h, --[no-]help", "--[no-]version", "--[no-]experimental", "--enable-feature="]

# Every prometheus server flag, exactly as --help-long renders it (bare name substring so a value
# flag `--name=DEFAULT` and a boolean `--[no-]name` both match the same token).
SERVER_FLAGS = [
    "config.file", "config.auto-reload-interval", "web.listen-address",
    "auto-gomaxprocs", "auto-gomemlimit", "auto-gomemlimit.ratio", "web.config.file",
    "web.read-timeout", "web.max-connections", "web.max-notifications-subscribers",
    "web.external-url", "web.route-prefix", "web.user-assets", "web.enable-lifecycle",
    "web.enable-admin-api", "web.enable-remote-write-receiver",
    "web.remote-write-receiver.accepted-protobuf-messages", "web.enable-otlp-receiver",
    "web.console.templates", "web.console.libraries", "web.page-title", "web.cors.origin",
    "storage.tsdb.path", "storage.tsdb.retention.time", "storage.tsdb.retention.size",
    "storage.tsdb.no-lockfile", "storage.tsdb.head-chunks-write-queue-size",
    "storage.tsdb.delay-compact-file.path", "storage.agent.path", "storage.agent.wal-compression",
    "storage.agent.retention.min-time", "storage.agent.retention.max-time",
    "storage.agent.no-lockfile", "storage.remote.flush-deadline", "storage.remote.read-sample-limit",
    "storage.remote.read-concurrent-limit", "storage.remote.read-max-bytes-in-frame",
    "rules.alert.for-outage-tolerance", "rules.alert.for-grace-period", "rules.alert.resend-delay",
    "rules.max-concurrent-evals", "alertmanager.notification-queue-capacity",
    "alertmanager.notification-batch-size", "alertmanager.drain-notification-queue-on-shutdown",
    "query.lookback-delta", "query.timeout", "query.max-concurrency", "query.max-samples",
    "enable-feature", "agent", "log.level", "log.format",
]
# A sample of the server's --enable-feature valid options (rendered inside the flag description).
SERVER_FEATURES = ["exemplar-storage", "memory-snapshot-on-shutdown", "promql-experimental-functions",
                   "otlp-deltatocumulative", "concurrent-rule-eval"]

# The full promtool leaf tree. Each entry: (argv, exact usage signature, leaf-specific+group flags).
# The signature is asserted BOTH in the root --help command listing AND as "usage: promtool <sig>"
# in the leaf's own --help; the flag tokens are asserted in the leaf's --help alongside the GLOBALS.
LEAVES = [
    (["check", "service-discovery"], "check service-discovery [<flags>] <config-file> <job>",
     ["--query.lookback-delta", "--timeout=", "<config-file>", "<job>"]),
    (["check", "config"], "check config [<flags>] <config-files>...",
     ["--query.lookback-delta", "--[no-]syntax-only", "--lint=", "--[no-]lint-fatal",
      "--[no-]ignore-unknown-fields", "--[no-]agent", "<config-files>"]),
    (["check", "web-config"], "check web-config <web-config-files>...",
     ["--query.lookback-delta", "<web-config-files>"]),
    (["check", "healthy"], "check healthy [<flags>]",
     ["--query.lookback-delta", "--http.config.file", "--url=http://localhost:9090"]),
    (["check", "ready"], "check ready [<flags>]",
     ["--query.lookback-delta", "--http.config.file", "--url=http://localhost:9090"]),
    (["check", "rules"], "check rules [<flags>] [<rule-files>...]",
     ["--query.lookback-delta", "--lint=", "--[no-]lint-fatal", "--[no-]ignore-unknown-fields",
      "<rule-files>"]),
    (["check", "metrics"], "check metrics [<flags>]",
     ["--query.lookback-delta", "--[no-]extended", "--lint="]),
    (["query", "instant"], "query instant [<flags>] <server> <expr>",
     ["--format=promql", "--http.config.file", "--time=", "<server>", "<expr>"]),
    (["query", "range"], "query range [<flags>] <server> <expr>",
     ["--format=promql", "--http.config.file", "--header=", "--start=", "--end=", "--step=",
      "<server>", "<expr>"]),
    (["query", "series"], "query series --match=MATCH [<flags>] <server>",
     ["--format=promql", "--http.config.file", "--match=", "--start=", "--end=", "<server>"]),
    (["query", "labels"], "query labels [<flags>] <server> <name>",
     ["--format=promql", "--http.config.file", "--start=", "--end=", "--match=", "<server>",
      "<name>"]),
    (["query", "analyze"], "query analyze --server=SERVER --type=TYPE --match=MATCH [<flags>]",
     ["--format=promql", "--http.config.file", "--server=", "--type=", "--duration=", "--time=",
      "--match="]),
    (["debug", "pprof"], "debug pprof <server>", ["<server>"]),
    (["debug", "metrics"], "debug metrics <server>", ["<server>"]),
    (["debug", "all"], "debug all <server>", ["<server>"]),
    (["push", "metrics"], "push metrics [<flags>] <remote-write-url> [<metric-files>...]",
     ["--http.config.file", "--label=", "--timeout=", "--header=", "--protobuf_message=",
      "<remote-write-url>", "<metric-files>"]),
    (["test", "rules"], "test rules [<flags>] <test-rule-file>...",
     ["--junit=", "--run=", "--[no-]debug", "--[no-]diff", "--[no-]ignore-unknown-fields",
      "<test-rule-file>"]),
    (["tsdb", "bench", "write"], "tsdb bench write [<flags>] [<file>]",
     ["--out=", "--metrics=", "--scrapes="]),
    (["tsdb", "analyze"], "tsdb analyze [<flags>] [<db path>] [<block id>]",
     ["--limit=", "--[no-]extended", "--match=", "<db path>", "<block id>"]),
    (["tsdb", "list"], "tsdb list [<flags>] [<db path>]",
     ["--[no-]human-readable", "<db path>"]),
    (["tsdb", "dump"], "tsdb dump [<flags>] [<db path>]",
     ["--sandbox-dir-root=", "--min-time=", "--max-time=", "--match=", "--format=", "<db path>"]),
    (["tsdb", "dump-openmetrics"], "tsdb dump-openmetrics [<flags>] [<db path>]",
     ["--sandbox-dir-root=", "--min-time=", "--max-time=", "--match=", "<db path>"]),
    (["tsdb", "create-blocks-from", "openmetrics"],
     "tsdb create-blocks-from openmetrics [<flags>] <input file> [<output directory>]",
     ["--[no-]human-readable", "--[no-]quiet", "--label=", "<input file>", "<output directory>"]),
    (["tsdb", "create-blocks-from", "rules"],
     "tsdb create-blocks-from rules --start=START [<flags>] <rule-files>...",
     ["--[no-]human-readable", "--[no-]quiet", "--http.config.file", "--url=", "--start=", "--end=",
      "--output-dir=", "--eval-interval=", "<rule-files>"]),
    (["promql", "format"], "promql format <query>", ["<query>"]),
    (["promql", "label-matchers", "set"], "promql label-matchers set [<flags>] <query> <name> <value>",
     ["--type=", "<query>", "<name>", "<value>"]),
    (["promql", "label-matchers", "delete"], "promql label-matchers delete <query> <name>",
     ["<query>", "<name>"]),
]


def check(cond, label):
    global _ok, _fail
    if cond:
        _ok += 1
    else:
        _fail += 1
        print("  FAIL %s" % label)


def run(args, timeout=90, input=None, cwd=None):
    r = subprocess.run(args, capture_output=True, text=True, timeout=timeout, input=input, cwd=cwd)
    return r.returncode, (r.stdout or "") + (r.stderr or "")


def http_get(path, timeout=8):
    try:
        with _OPENER.open("http://%s%s" % (EP, path), timeout=timeout) as r:
            return r.getcode(), r.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode("utf-8", "replace")
    except Exception:
        return 0, ""


def help_tree():
    # prometheus (kingpin) prints a curated set under --help and the EXHAUSTIVE flag tree under
    # --help-long; assert both work and check every documented server flag against --help-long.
    rc0, _ = run([PROM, "--help"])
    check(rc0 == 0, "prometheus --help exits 0")
    rc, ph = run([PROM, "--help-long"])
    check(rc == 0, "prometheus --help-long exits 0")
    for flag in SERVER_FLAGS:
        check(flag in ph, "prometheus --help-long documents %s" % flag)
    for feat in SERVER_FEATURES:
        check(feat in ph, "prometheus --enable-feature lists %s" % feat)

    # promtool root --help: the seven command groups + `help`, the persistent globals, and the exact
    # usage signature of every one of the 27 leaves (proves the whole command tree from the top).
    rc, th = run([PROMTOOL, "--help"])
    check(rc == 0, "promtool --help exits 0")
    for grp in ["check", "query", "debug", "test", "push", "tsdb", "promql"]:
        check(("\n%s " % grp) in th or ("\n%s\n" % grp) in th or (" %s " % grp) in th,
              "promtool --help lists group %s" % grp)
    check("help [<command>...]" in th, "promtool --help lists the help command")
    for g in GLOBALS:
        check(g in th, "promtool --help documents global %s" % g)
    for argv, sig, _flags in LEAVES:
        check(sig in th, "promtool --help lists leaf: %s" % sig)

    # every leaf's own --help: usage line + persistent globals + its full documented flag surface.
    for argv, sig, flags in LEAVES:
        name = " ".join(argv)
        rc, h = run([PROMTOOL] + argv + ["--help"])
        check(rc == 0, "promtool %s --help exits 0" % name)
        check(("usage: promtool " + sig) in h, "promtool %s --help usage line" % name)
        for g in GLOBALS:
            check(g in h, "promtool %s --help documents global %s" % (name, g))
        for tok in flags:
            check(tok in h, "promtool %s --help documents %s" % (name, tok))


def functional_cli(work):
    # ---- check config: the real target config validates; a bad duration is rejected non-zero. ----
    rc, out = run([PROMTOOL, "check", "config", CFG])
    check(rc == 0 and "SUCCESS" in out, "promtool check config %s -> SUCCESS" % CFG)
    badcfg = os.path.join(work, "bad.yml")
    with open(badcfg, "w") as f:
        f.write("global:\n  scrape_interval: notaduration\n")
    rc, out = run([PROMTOOL, "check", "config", badcfg])
    check(rc != 0 and "FAILED" in out, "promtool check config (bad duration) -> non-zero + FAILED")

    # ---- check rules: a well-formed group validates; an unbalanced expr is rejected non-zero. ----
    goodrules = os.path.join(work, "rules.yml")
    with open(goodrules, "w") as f:
        f.write("groups:\n  - name: example\n    rules:\n"
                "      - record: job:up:sum\n        expr: sum(up) by (job)\n"
                "      - alert: InstanceDown\n        expr: up == 0\n        for: 5m\n")
    rc, out = run([PROMTOOL, "check", "rules", goodrules])
    check(rc == 0 and "SUCCESS" in out, "promtool check rules (valid group) -> SUCCESS")
    badrules = os.path.join(work, "bad-rules.yml")
    with open(badrules, "w") as f:
        f.write("groups:\n  - name: bad\n    rules:\n"
                "      - record: job:up:sum\n        expr: sum(up) by (job\n")
    rc, out = run([PROMTOOL, "check", "rules", badrules])
    check(rc != 0 and "FAILED" in out, "promtool check rules (unclosed paren) -> non-zero + FAILED")

    # ---- check metrics: reads a metrics exposition over stdin; a counter without _total lints. ----
    rc, out = run([PROMTOOL, "check", "metrics"],
                  input="# HELP a_total h\n# TYPE a_total counter\na_total 1\n")
    check(rc == 0, "promtool check metrics (well-named) -> exit 0 over stdin")
    rc, out = run([PROMTOOL, "check", "metrics"],
                  input="# HELP my_metric h\n# TYPE my_metric counter\nmy_metric 1\n")
    check(rc != 0 and "_total" in out, "promtool check metrics (counter without _total) -> lint error")

    # ---- check web-config: a basic_auth users file validates; a missing cert is rejected. ----
    goodweb = os.path.join(work, "web.yml")
    with open(goodweb, "w") as f:
        f.write("basic_auth_users:\n  admin: $2y$10$" + "a" * 53 + "\n")
    rc, out = run([PROMTOOL, "check", "web-config", goodweb])
    check(rc == 0 and "SUCCESS" in out, "promtool check web-config (basic_auth) -> SUCCESS")
    badweb = os.path.join(work, "webbad.yml")
    with open(badweb, "w") as f:
        f.write("tls_server_config:\n  cert_file: /nonexistent/x.crt\n  key_file: /nonexistent/x.key\n")
    rc, out = run([PROMTOOL, "check", "web-config", badweb])
    check(rc != 0 and "FAILED" in out, "promtool check web-config (missing cert) -> non-zero + FAILED")

    # ---- promql (the --experimental PromQL rewriters; pure functions, no server). ----
    rc, out = run([PROMTOOL, "--experimental", "promql", "format", 'up{job="x"}+1'])
    check(rc == 0 and 'up{job="x"} + 1' in out, "promtool promql format pretty-prints the query")
    rc, out = run([PROMTOOL, "--experimental", "promql", "format", "up{{"])
    check(rc != 0 and "parse error" in out, "promtool promql format (bad query) -> parse error")
    rc, out = run([PROMTOOL, "--experimental", "promql", "label-matchers", "set", "up", "foo", "bar"])
    check(rc == 0 and 'foo="bar"' in out, "promtool promql label-matchers set adds foo=\"bar\"")
    rc, out = run([PROMTOOL, "--experimental", "promql", "label-matchers", "delete",
                   'up{foo="bar"}', "foo"])
    check(rc == 0 and "up" in out and "foo" not in out,
          "promtool promql label-matchers delete removes foo")

    # ---- test rules: a rule unit-test drives the PromQL engine over synthetic input_series. ----
    with open(os.path.join(work, "alert_rules.yml"), "w") as f:
        f.write("groups:\n  - name: example\n    rules:\n"
                "      - alert: InstanceDown\n        expr: up == 0\n        for: 1m\n"
                "        labels:\n          severity: page\n")
    with open(os.path.join(work, "unittest.yml"), "w") as f:
        f.write(
            "rule_files:\n  - alert_rules.yml\nevaluation_interval: 1m\ntests:\n"
            "  - interval: 1m\n    input_series:\n"
            "      - series: 'up{job=\"prometheus\", instance=\"localhost:9090\"}'\n"
            "        values: '0 0 0 0 0'\n"
            "    alert_rule_test:\n"
            "      - eval_time: 3m\n        alertname: InstanceDown\n        exp_alerts:\n"
            "          - exp_labels:\n              severity: page\n"
            "              job: prometheus\n              instance: localhost:9090\n")
    rc, out = run([PROMTOOL, "test", "rules", "unittest.yml"], cwd=work)
    check(rc == 0 and "SUCCESS" in out, "promtool test rules (alert unit-test) -> SUCCESS")

    # ---- TSDB import path: OpenMetrics -> blocks, then list + analyze the produced block. This is
    #      the server-free way to exercise block writing + WAL replay + index build on the ext4 image
    #      (tsdb dump/dump-openmetrics need a data dir WITH a live WAL, which only the running server
    #      produces and tears down, so those two stay --help-level above).
    om = os.path.join(work, "om.txt")
    with open(om, "w") as f:
        f.write("# HELP test_metric A test metric\n# TYPE test_metric gauge\n"
                'test_metric{foo="bar"} 1 1600000000\n'
                'test_metric{foo="bar"} 2 1600000060\n'
                'test_metric{foo="bar"} 3 1600000120\n# EOF\n')
    blocks = os.path.join(work, "blocks")
    rc, out = run([PROMTOOL, "tsdb", "create-blocks-from", "openmetrics", om, blocks], timeout=180)
    # create-blocks-from does not echo the block table to stdout in 3.11.3; the block IS created on
    # disk. Verify creation via the block dir; the following `tsdb list` confirms the ULID is readable.
    check(rc == 0 and os.path.isdir(blocks) and len(os.listdir(blocks)) >= 1,
          "promtool tsdb create-blocks-from openmetrics -> creates a block (rc=0 + block dir)")
    rc, out = run([PROMTOOL, "tsdb", "list", blocks], timeout=60)
    check(rc == 0 and "BLOCK ULID" in out, "promtool tsdb list -> lists the produced block")
    rc, out = run([PROMTOOL, "tsdb", "analyze", blocks], timeout=120)
    check(rc == 0 and "Total Series" in out, "promtool tsdb analyze -> Total Series report")


def main():
    print("=== PrometheusCarpet: prometheus %s + node_exporter %s end-to-end ===" % (VER, NE_VER))
    for b in (PROM, PROMTOOL, NODE_EXP):
        check(os.path.exists(b), "binary present: %s" % b)
    if _fail:
        print("PROM_RESULT ok=%d fail=%d" % (_ok, _fail))
        print("--- binaries missing; cannot run ---")
        return 1

    # --- VER red-line ---
    rc, out = run([PROM, "--version"])
    check(rc == 0 and re.search(r"^prometheus, version %s " % re.escape(VER), out, re.M) is not None,
          "prometheus --version == %s (red-line)" % VER)
    rc, out = run([PROMTOOL, "--version"])
    check(rc == 0 and re.search(r"version %s" % re.escape(VER), out) is not None,
          "promtool --version == %s (red-line)" % VER)
    rc, out = run([NODE_EXP, "--version"])
    check(rc == 0 and NE_VER in out, "node_exporter --version == %s" % NE_VER)

    # --- HELP tree (carpet-level: every command x subcommand x flag) ---
    help_tree()

    # --- FUNC: deterministic, server-free promtool behaviors ---
    work = tempfile.mkdtemp(prefix="promtool-func-", dir="/root")
    try:
        functional_cli(work)
    finally:
        shutil.rmtree(work, ignore_errors=True)

    # --- launch node_exporter (the scrape target) ---
    ne_log = open("/tmp/node_exporter.log", "w")
    ne = subprocess.Popen(
        [NODE_EXP, "--web.listen-address=%s" % NE_EP,
         "--collector.disable-defaults",
         "--collector.uname", "--collector.cpu", "--collector.meminfo", "--collector.loadavg",
         "--collector.netdev", "--no-collector.netdev.netlink",
         "--collector.diskstats", "--collector.filesystem"],
        stdout=ne_log, stderr=subprocess.STDOUT)
    ne_up = False
    for _ in range(40):
        if ne.poll() is not None:
            break
        try:
            with _OPENER.open("http://%s/metrics" % NE_EP, timeout=3) as r:
                if r.getcode() == 200 and len(r.read()) > 0:
                    ne_up = True
                    break
        except Exception:
            pass
        time.sleep(1)
    check(ne_up, "node_exporter serving /metrics on %s" % NE_EP)

    # --- launch prometheus headless ---
    if os.path.isdir(TSDB):
        shutil.rmtree(TSDB, ignore_errors=True)
    os.makedirs(TSDB, exist_ok=True)
    p_log = "/tmp/prometheus.log"
    plog = open(p_log, "w")
    prom = subprocess.Popen(
        [PROM, "--config.file=%s" % CFG, "--web.listen-address=%s" % EP,
         "--storage.tsdb.path=%s" % TSDB, "--web.enable-lifecycle"],
        stdout=plog, stderr=subprocess.STDOUT)
    try:
        # READY
        ready = False
        for _ in range(300):
            if prom.poll() is not None:
                break
            try:
                log = open(p_log).read()
            except Exception:
                log = ""
            if "Server is ready to receive web requests" in log:
                code, body = http_get("/-/ready", timeout=4)
                if code == 200 and "Ready" in body:
                    ready = True
                    break
            time.sleep(1)
        check(ready, "log 'Server is ready' + /-/ready answers Ready")
        if not ready:
            try:
                print("--- prometheus.log tail ---\n" + open(p_log).read()[-1500:])
            except Exception:
                pass

        # QUERY: PromQL engine round-trips vector(42)
        query_ok = False
        if ready:
            code, body = http_get("/api/v1/query?query=vector(42)", timeout=6)
            query_ok = code == 200 and '"status":"success"' in body and '"42"' in body
        check(query_ok, "/api/v1/query vector(42) -> success + value 42 (PromQL engine)")

        # PROMTOOL as a PromQL client against the live server
        if ready:
            rc, out = run([PROMTOOL, "query", "instant", "http://%s" % EP, "vector(42)"], timeout=30)
            check(rc == 0 and "42" in out, "promtool query instant vector(42) -> 42 (PromQL client)")
        else:
            check(False, "promtool query instant (server not ready)")

        # SCRAPE: prometheus scraped node_exporter -> up{job=node} == 1
        scrape_ok = False
        if ready and ne_up:
            for _ in range(240):
                code, body = http_get(
                    '/api/v1/query?query=up%7Bjob%3D%22node%22%7D', timeout=8)
                if code == 200 and '"status":"success"' in body and '"1"]' in body:
                    scrape_ok = True
                    break
                time.sleep(1)
        check(scrape_ok, "up{job=node} == 1 (node_exporter scraped + ingested through :9090->:9100)")

        # promtool query labels/series as a live PromQL client, against the ingested `up` series.
        labels_ok = series_ok = False
        if scrape_ok:
            rc, out = run([PROMTOOL, "query", "labels", "http://%s" % EP, "__name__"], timeout=30)
            labels_ok = rc == 0 and "up" in out
            rc, out = run([PROMTOOL, "query", "series", "--match=up", "http://%s" % EP], timeout=30)
            series_ok = rc == 0 and 'job="node"' in out
        check(labels_ok, "promtool query labels __name__ -> lists 'up' (live PromQL client)")
        check(series_ok, "promtool query series --match=up -> job=node (live PromQL client)")

        # ===== INTEGRATION SOAK: scrape -> store -> query end-to-end =====
        # Let prometheus scrape node_exporter across a multi-minute soak (2s interval), then verify
        # node_exporter's OWN metrics landed in the TSDB and a range query shows many samples across
        # the whole window (proves multi-round scrape + persistence, not a single lucky point).
        soak_ok = cpu_ok = mem_ok = range_ok = False
        SOAK = int(os.environ.get("PROM_SOAK_SECS", "180"))
        if scrape_ok:
            t0 = int(time.time())
            time.sleep(SOAK)
            t1 = int(time.time())
            soak_ok = True
            # node_cpu_seconds_total: node_exporter counter (from /proc/stat) must be in the TSDB.
            code, body = http_get("/api/v1/query?query=node_cpu_seconds_total", timeout=8)
            cpu_ok = (code == 200 and '"status":"success"' in body
                      and "node_cpu_seconds_total" in body
                      and re.search(r'"value":\[[0-9.]+,"[0-9]', body) is not None)
            check(cpu_ok, "node_cpu_seconds_total ingested with numeric samples (node_exporter -> TSDB)")
            # node_memory_MemTotal_bytes: node_exporter gauge (from /proc/meminfo).
            code, body = http_get("/api/v1/query?query=node_memory_MemTotal_bytes", timeout=8)
            mem_ok = (code == 200 and '"status":"success"' in body
                      and re.search(r'"value":\[[0-9.]+,"[0-9]', body) is not None)
            check(mem_ok, "node_memory_MemTotal_bytes ingested with numeric value (node_exporter -> TSDB)")
            # node_network_receive_bytes_total: procfs-backed counter (from /proc/net/dev) -- proves
            # the netdev collector's data flowed scrape->store->query, not just cpu/meminfo.
            code, body = http_get('/api/v1/query?query=node_network_receive_bytes_total', timeout=8)
            net_ok = (code == 200 and '"status":"success"' in body
                      and re.search(r'"value":\[[0-9.]+,"[0-9]', body) is not None)
            check(net_ok, "node_network_receive_bytes_total ingested (netdev collector -> TSDB)")
            # query_range over the soak window: many up{job=node} samples proves multi-round scraping.
            q = ("/api/v1/query_range?query=up%%7Bjob%%3D%%22node%%22%%7D"
                 "&start=%d&end=%d&step=15") % (t0, t1)
            code, body = http_get(q, timeout=12)
            npts = (body.count("],[") + 1) if '"values":[' in body else 0
            range_ok = code == 200 and '"status":"success"' in body and npts >= 5
            check(range_ok, "query_range up{job=node} over %ds -> %d samples (multi-round scrape+store)"
                  % (SOAK, npts))
        check(soak_ok, "prometheus soaked node_exporter scrapes for %ds (scrape->store)" % SOAK)
    finally:
        for p in (prom, ne):
            try:
                p.send_signal(signal.SIGTERM)
                p.wait(timeout=6)
            except Exception:
                try:
                    p.kill()
                except Exception:
                    pass
        plog.close()
        ne_log.close()

    print("PROM_RESULT ok=%d fail=%d" % (_ok, _fail))
    if _fail == 0:
        print("PROM_DONE")
        return 0
    return 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as e:
        import traceback
        traceback.print_exc()
        print("PROM_RESULT ok=%d fail=%d" % (_ok, _fail + 1))
        sys.exit(1)
