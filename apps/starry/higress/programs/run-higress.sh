#!/bin/sh
# run-higress.sh - higress standalone gateway carpet on StarryOS.
#
# Brings up self-contained upstreams (the static `echod` HTTP echo backends and
# an openssl s_server TLS upstream), starts Envoy against the static bootstrap,
# and asserts the documented gateway data path end to end: the CLI surface, the
# admin endpoints, every route-match kind (prefix / exact path / safe_regex /
# query-parameter / header), weighted + round-robin / least-request / random load
# balancing, request + response header mutation (add + remove), path rewrite
# (prefix + regex capture), host rewrite, redirect + direct-response actions,
# per-route timeout, upstream retry, per-route local rate limiting, dead-upstream
# and TLS-verify exceptions, and downstream + upstream TLS. It then restarts
# Envoy against the reuse_port:true bootstrap to prove SO_REUSEPORT is accepted
# by the kernel (Envoy defaults enable_reuse_port=true on Linux, so without the
# kernel option the listener socket would fail with ENOPROTOOPT).
#
# Emits HIGRESS_OK=<pass>/<total> and, only when every one of the expected
# assertions passed, TEST PASSED. The trailing marker is printed by a guarded
# printf so the count cannot be forged by a partial run.
set -u

WORK=/tmp/higress
rm -rf "$WORK"; mkdir -p "$WORK"
CONF_DIR=/etc/higress
CERT="$CONF_DIR/certs/server.crt"
KEY="$CONF_DIR/certs/server.key"
export LD_LIBRARY_PATH="/lib:/lib64:/usr/lib:${LD_LIBRARY_PATH:-}"
export PATH="/usr/bin:/bin:/sbin:/usr/sbin"

EXPECTED=65
PASS=0; TOTAL=0
ok()  { TOTAL=$((TOTAL+1)); PASS=$((PASS+1)); echo "  PASS | $1"; }
bad() { TOTAL=$((TOTAL+1)); echo "  FAIL | $1"; }
ck_eq()  { if [ "$2" = "$3" ]; then ok "$1 | $2"; else bad "$1 | want[$3] got[$2]"; fi; }
ck_ge()  { if [ "${2:-0}" -ge "$3" ] 2>/dev/null; then ok "$1 | $2>=$3"; else bad "$1 | ${2:-0} not>=$3"; fi; }
ck_has() { if printf '%s' "$2" | grep -q -- "$3"; then ok "$1"; else bad "$1 | missing[$3]"; fi; }
ck_no()  { if printf '%s' "$2" | grep -q -- "$3"; then bad "$1 | unexpected[$3]"; else ok "$1"; fi; }
ck_5xx() { case "$2" in 5??) ok "$1 | $2";; *) bad "$1 | want5xx got[$2]";; esac; }

# --- HTTP client helpers (busybox wget, plaintext listener :10000 / admin :9901) ---
hb() { wget -q -O - -T 10 "http://127.0.0.1:10000$1" 2>/dev/null; }
hh() { wget -S -O /dev/null -T 10 "http://127.0.0.1:10000$1" 2>&1; }
hc() { hh "$1" | grep -oE 'HTTP/1\.[01] [0-9][0-9][0-9]' | head -1 | awk '{print $2}'; }
ab() { wget -q -O - -T 10 "http://127.0.0.1:9901$1" 2>/dev/null; }
ac() { wget -S -O /dev/null -T 10 "http://127.0.0.1:9901$1" 2>&1 | grep -oE 'HTTP/1\.[01] [0-9][0-9][0-9]' | head -1 | awk '{print $2}'; }

# --- TLS client helpers (openssl s_client, downstream TLS listener :10443) ---
# tlsq METHOD PATH [EXTRA-HEADER]  -> full HTTP response (status line + headers + body)
tlsq() {
    { printf '%s %s HTTP/1.1\r\n' "$1" "$2"
      printf 'Host: localhost\r\n'
      [ -n "${3:-}" ] && printf '%s\r\n' "$3"
      printf 'Connection: close\r\n\r\n'
    } | openssl s_client -connect 127.0.0.1:10443 -quiet 2>/dev/null
}
tlsinfo() { openssl s_client -connect 127.0.0.1:10443 -servername localhost </dev/null 2>&1; }

wait_ready() { # tries
    i=0
    while [ "$i" -lt "${1:-60}" ]; do
        [ "$(ab /ready)" = "LIVE" ] && return 0
        sleep 1; i=$((i+1))
    done
    return 1
}
start_envoy() { # config base-id
    /usr/bin/envoy -c "$1" --concurrency 1 --disable-hot-restart --base-id "$2" \
        >"$WORK/envoy-$2.log" 2>&1 &
    echo "$!"
}

echo "=== higress standalone gateway carpet ==="
ENVOY_BIN=/usr/bin/envoy

# --- 1. CLI surface (envoy --version / --mode validate / --help) ---
ver=$("$ENVOY_BIN" --version 2>&1)
ck_has "cli: version 1.38.3"        "$ver" "1.38.3"
ck_has "cli: version keyword"       "$ver" "version"
"$ENVOY_BIN" --mode validate -c "$CONF_DIR/bootstrap.yaml" >"$WORK/val.log" 2>&1
ck_eq  "cli: validate good rc0"     "$?" "0"
printf 'nonsense: [\n' > "$WORK/bad.yaml"
"$ENVOY_BIN" --mode validate -c "$WORK/bad.yaml" >/dev/null 2>&1
if [ "$?" -ne 0 ]; then ok "cli: validate bad rc!=0"; else bad "cli: validate bad rc!=0"; fi
ck_has "cli: help mentions concurrency" "$("$ENVOY_BIN" --help 2>&1)" "concurrency"

# --- 2. upstreams (echod HTTP backends + openssl s_server TLS upstream) ---
/usr/bin/echod 8081 backend_a    ok      >"$WORK/a.log"    2>&1 &
/usr/bin/echod 8082 backend_b    ok      >"$WORK/b.log"    2>&1 &
/usr/bin/echod 8083 backend_c    ok      >"$WORK/c.log"    2>&1 &
/usr/bin/echod 8084 backend_fail fail503 >"$WORK/f.log"    2>&1 &
/usr/bin/echod 8085 backend_slow slow    >"$WORK/s.log"    2>&1 &
openssl s_server -accept 8443 -cert "$CERT" -key "$KEY" -www -quiet >"$WORK/sserver.log" 2>&1 &
sleep 2

# --- 3. baseline Envoy (enable_reuse_port:false) ---
envoy_pid=$(start_envoy "$CONF_DIR/bootstrap.yaml" 1)
if wait_ready 60; then
    # admin endpoints
    ck_eq  "admin: /ready LIVE"            "$(ab /ready)" "LIVE"
    ck_has "admin: /stats server.state"    "$(ab /stats)" "server.state"
    ck_has "admin: /server_info LIVE"      "$(ab /server_info)" "LIVE"
    ck_has "admin: /clusters backend_a"    "$(ab /clusters)" "backend_a"
    listeners=$(ab /listeners)
    ck_has "admin: /listeners http"        "$listeners" "0.0.0.0:10000"
    ck_has "admin: /listeners https"       "$listeners" "0.0.0.0:10443"
    filtered=$(ab '/stats?filter=server.state')
    ck_has "admin: /stats?filter includes" "$filtered" "server.state"
    ck_no  "admin: /stats?filter excludes" "$filtered" "backend_a"
    ck_has "admin: /stats prometheus"      "$(ab '/stats?format=prometheus')" "envoy_server_"
    ck_has "admin: /config_dump node id"   "$(ab /config_dump)" "higress-standalone"
    ck_has "admin: /certs localhost"       "$(ab /certs)" "localhost"
    ck_eq  "admin: unknown path 404"       "$(ac /no_such_admin_endpoint)" "404"

    # route matching
    ck_has "route: / -> backend_a"         "$(hb /)" "BACKEND=backend_a"
    ck_has "match: exact /exact"           "$(hh /exact)" "x-route: exact"
    ck_no  "match: exact non-match"        "$(hh /exactZZ)" "x-route: exact"
    ck_has "match: safe_regex /img/a.png"  "$(hh /img/a.png)" "x-route: regex"
    ck_no  "match: safe_regex non-.png"    "$(hh /img/a.txt)" "x-route: regex"
    ck_has "match: query ver=v2"           "$(hh '/qp?ver=v2')" "x-route: qp-v2"
    ck_has "match: query default"          "$(hh '/qp?ver=v1')" "x-route: qp-default"

    # rewrite actions
    rw=$(hb /api/foo/bar)
    ck_has "rewrite: prefix -> /echo"      "$rw" "/echo/foo/bar"
    ck_no  "rewrite: strips /api prefix"   "$rw" "/api"
    ck_has "rewrite: regex capture orig="  "$(hb /rgx/hello)" "orig=hello"
    ck_has "rewrite: host_rewrite_literal" "$(hb /hostrw)" "HOST=rewritten.example"

    # header mutation
    ck_has "header: request add"           "$(hb /api/x)" "X_HIGRESS_ADDED=starry"
    ck_has "header: response add"          "$(hh /api/x)" "x-higress-gateway: envoy-standalone"
    ck_has "header: response present /leak" "$(hh /leak)" "x-backend-secret: leak"
    ck_no  "header: response removed"      "$(hh /nosecret)" "x-backend-secret"

    # redirect / direct-response
    ck_eq  "action: redirect 301"          "$(hc /redir)" "301"
    ck_has "action: redirect Location"     "$(hh /redir)" "127.0.0.1:10000/"
    ck_eq  "action: direct /ping 200"      "$(hc /ping)" "200"
    ck_has "action: direct /ping body"     "$(hb /ping)" "PONG"
    nope=$(tlsq GET /nope )
    ck_has "action: direct /nope 404"      "$nope" "404"
    ck_has "action: direct /nope body"     "$nope" "NO_ROUTE"

    # load balancing
    rr=""; i=0; while [ "$i" -lt 12 ]; do rr="$rr$(hb /rr)"; i=$((i+1)); done
    ck_has "lb: round_robin hits a"        "$rr" "BACKEND=backend_a"
    ck_has "lb: round_robin hits b"        "$rr" "BACKEND=backend_b"
    ca=0; cb=0; i=0
    while [ "$i" -lt 40 ]; do
        r=$(hb /api/x)
        case "$r" in *backend_a*) ca=$((ca+1)) ;; esac
        case "$r" in *backend_b*) cb=$((cb+1)) ;; esac
        i=$((i+1))
    done
    echo "  (weighted tally: a=$ca b=$cb)"
    ck_ge "lb: weighted a hits"            "$ca" "1"
    ck_ge "lb: weighted b hits"            "$cb" "1"
    if [ "$ca" -gt "$cb" ]; then ok "lb: weighted a>b ($ca>$cb)"; else bad "lb: weighted a=$ca !> b=$cb"; fi
    ck_has "lb: least_request serves"      "$(hb /lr)" "BACKEND=backend_"
    ck_has "lb: random serves"             "$(hb /rand)" "BACKEND=backend_"

    # retry / rate limit
    ck_eq "retry: /flaky final 503"        "$(hc /flaky)" "503"
    retries=$(ab /stats | sed -n 's/^cluster.backend_fail.upstream_rq_retry: //p')
    ck_ge "retry: upstream_rq_retry"       "${retries:-0}" "1"
    c200=0; c429=0; i=0
    while [ "$i" -lt 5 ]; do
        code=$(hc /limited)
        [ "$code" = "200" ] && c200=$((c200+1))
        [ "$code" = "429" ] && c429=$((c429+1))
        i=$((i+1))
    done
    echo "  (ratelimit tally: 200=$c200 429=$c429)"
    ck_ge "ratelimit: 200 allowed"         "$c200" "1"
    ck_ge "ratelimit: 429 throttled"       "$c429" "1"

    # exceptions
    ck_eq  "exception: dead upstream 503"  "$(hc /dead)" "503"
    ck_eq  "exception: route timeout 504"  "$(hc /slow)" "504"
    ck_5xx "exception: upstream TLS verify fail" "$(hc /secure-verify)"

    # TLS (upstream + downstream)
    ck_has "tls: upstream /secure body"    "$(hb /secure)" "Ciphers supported in s_server"
    ck_eq  "tls: upstream /secure code"    "$(hc /secure)" "200"
    ck_has "tls: downstream -> backend_a"  "$(tlsq GET / )" "BACKEND=backend_a"
    info=$(tlsinfo)
    ck_has "tls: downstream cert localhost" "$info" "localhost"
    if printf '%s' "$info" | grep -qE 'New, TLSv1\.(2|3)'; then ok "tls: downstream TLSv1.2+"; else bad "tls: downstream TLSv1.2+"; fi

    # header-match / method / header-strip / integration (via downstream TLS listener)
    ck_has "match: header x-canary=on"     "$(tlsq GET / 'X-Canary: on')" "x-route: canary"
    ck_no  "match: header absent -> default" "$(tlsq GET / )" "x-route: canary"
    ck_has "method: POST reflected"        "$(tlsq POST / )" "METHOD=POST"
    ck_has "method: PUT reflected"         "$(tlsq PUT / )" "METHOD=PUT"
    ck_has "header: request strip"         "$(tlsq GET / 'X-Strip-Me: yes')" "X_STRIP_ME=$"
    ck_has "integration: TLS-in-TLS /secure" "$(tlsq GET /secure )" "Ciphers supported in s_server"
else
    echo "  FAIL | baseline Envoy did not become ready"
    tail -40 "$WORK/envoy-1.log" 2>/dev/null || true
fi
kill "$envoy_pid" 2>/dev/null || true
sleep 2

# --- 4. reuse_port:true (exercises SO_REUSEPORT) ---
envoy_rp_pid=$(start_envoy "$CONF_DIR/bootstrap-reuseport.yaml" 2)
if wait_ready 60; then
    ck_eq  "reuse_port: /ready LIVE"       "$(ab /ready)" "LIVE"
    ck_has "reuse_port: listener serves"   "$(hb /)" "BACKEND=backend_a"
else
    echo "  FAIL | reuse_port Envoy did not become ready (SO_REUSEPORT?)"
    tail -40 "$WORK/envoy-2.log" 2>/dev/null || true
    TOTAL=$((TOTAL+2))
fi
kill "$envoy_rp_pid" 2>/dev/null || true

echo "HIGRESS_OK=$PASS/$TOTAL"
if [ "$PASS" -eq "$EXPECTED" ] && [ "$TOTAL" -eq "$EXPECTED" ]; then
    printf 'TEST %s\n' "PASSED"
else
    printf 'TEST %s\n' "FAILED"
fi
