#!/bin/sh
# run-higress.sh - higress standalone gateway carpet on StarryOS.
#
# Brings up self-contained backends (busybox httpd CGI + openssl s_server for the
# TLS upstream), starts Envoy against the static bootstrap, and asserts the full
# gateway data path: prefix/host routing, weighted load balancing, request +
# response header mutation, path rewrite, upstream retry, per-route local rate
# limiting, downstream + upstream TLS, and the admin endpoint. It then restarts
# Envoy against the reuse_port:true bootstrap to prove SO_REUSEPORT is accepted
# by the kernel (Envoy defaults enable_reuse_port=true on Linux, so without the
# kernel option the listener socket would fail with ENOPROTOOPT).
#
# Emits HIGRESS_OK=<pass>/<total> and, only when every assertion of the expected
# set passed, TEST PASSED. The trailing marker is printed by a guarded printf so
# the count cannot be forged by a partial run.
set -u

WORK=/tmp/higress
rm -rf "$WORK"; mkdir -p "$WORK"
CONF_DIR=/etc/higress
CERT="$CONF_DIR/certs/server.crt"
KEY="$CONF_DIR/certs/server.key"
export LD_LIBRARY_PATH="/lib:/lib64:${LD_LIBRARY_PATH:-}"

EXPECTED=18
PASS=0; TOTAL=0
check_eq()  { TOTAL=$((TOTAL+1)); if [ "$2" = "$3" ]; then PASS=$((PASS+1)); echo "  PASS | $1 | $2"; else echo "  FAIL | $1 | expected[$3] got[$2]"; fi; }
check_ge()  { TOTAL=$((TOTAL+1)); if [ "${2:-0}" -ge "$3" ] 2>/dev/null; then PASS=$((PASS+1)); echo "  PASS | $1 | $2>=$3"; else echo "  FAIL | $1 | ${2:-0} not >= $3"; fi; }
check_has() { TOTAL=$((TOTAL+1)); if printf '%s' "$2" | grep -q -- "$3"; then PASS=$((PASS+1)); echo "  PASS | $1"; else echo "  FAIL | $1 | missing[$3]"; fi; }
check_no()  { TOTAL=$((TOTAL+1)); if printf '%s' "$2" | grep -q -- "$3"; then echo "  FAIL | $1 | unexpected[$3]"; else PASS=$((PASS+1)); echo "  PASS | $1"; fi; }

# --- apk: openssl is needed for downstream TLS client + upstream TLS backend ---
apk_add_openssl() {
    command -v openssl >/dev/null 2>&1 && return 0
    branch="latest-stable"
    [ -r /etc/alpine-release ] && branch="v$(cut -d. -f1,2 /etc/alpine-release)"
    for m in "https://mirrors.tuna.tsinghua.edu.cn/alpine" "https://dl-cdn.alpinelinux.org/alpine"; do
        printf '%s/%s/main\n%s/%s/community\n' "$m" "$branch" "$m" "$branch" > "$WORK/repos"
        if timeout 120 apk --no-progress --update-cache --repositories-file "$WORK/repos" add openssl >"$WORK/apk.log" 2>&1; then
            echo "HIGRESS_APK_OK: $m/$branch"
            return 0
        fi
        echo "HIGRESS_APK_FAIL: $m/$branch"
    done
    return 1
}

# --- backends ---------------------------------------------------------------
# busybox httpd runs files under cgi-bin/ as CGI and exports REQUEST_URI (the
# full received path), REQUEST_METHOD and HTTP_<HEADER> vars, so the echo CGI
# proves path rewrite (REQUEST_URI) and request-header injection (HTTP_*).
make_http_backend() {
    id="$1"; port="$2"; status="$3"
    d="$WORK/$id"; mkdir -p "$d/cgi-bin"
    printf 'BACKEND=%s\n' "$id" > "$d/index.html"
    if [ "$status" = "503" ]; then
        cat > "$d/cgi-bin/echo" <<EOF
#!/bin/sh
echo "Status: 503 Service Unavailable"
echo "Content-Type: text/plain"
echo ""
echo "BACKEND=$id"
EOF
    else
        cat > "$d/cgi-bin/echo" <<EOF
#!/bin/sh
echo "Content-Type: text/plain"
echo ""
echo "BACKEND=$id"
echo "PATH_INFO=\${REQUEST_URI}"
echo "METHOD=\${REQUEST_METHOD}"
echo "X_HIGRESS_ADDED=\${HTTP_X_HIGRESS_ADDED}"
EOF
    fi
    chmod +x "$d/cgi-bin/echo"
    busybox httpd -p "127.0.0.1:$port" -h "$d"
}

# --- http client helpers (busybox wget) -------------------------------------
http_body()    { busybox wget -q -O - "$1" 2>/dev/null || true; }
http_headers() { busybox wget -S -q -O /dev/null "$1" 2>&1 || true; }
http_code()    {
    busybox wget -S -q -O /dev/null "$1" 2>"$WORK/hc" || true
    sed -n 's#^[[:space:]]*HTTP/[0-9.]*[[:space:]]\([0-9][0-9][0-9]\).*#\1#p' "$WORK/hc" | tail -1
}
tls_body() { # host port path
    printf 'GET %s HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n' "$3" \
        | openssl s_client -connect "$1:$2" -quiet 2>/dev/null || true
}

wait_ready() { # tries
    i=0
    while [ "$i" -lt "${1:-40}" ]; do
        [ "$(http_body http://127.0.0.1:9901/ready)" = "LIVE" ] && return 0
        sleep 1; i=$((i+1))
    done
    return 1
}

start_envoy() { # config base-id
    /usr/bin/envoy -c "$1" --concurrency 1 --disable-hot-restart --base-id "$2" \
        >"$WORK/envoy-$2.log" 2>&1 &
    echo "$!"
}

# ---------------------------------------------------------------------------
echo "=== higress standalone gateway carpet ==="
apk_add_openssl || echo "HIGRESS_WARN: openssl not installed (TLS assertions will fail)"

make_http_backend backend_a 8081 200
make_http_backend backend_b 8082 200
make_http_backend backend_c 8083 200
make_http_backend backend_fail 8084 503
openssl s_server -accept 8443 -cert "$CERT" -key "$KEY" -www -quiet >"$WORK/sserver.log" 2>&1 &
sleep 2

# --- baseline (enable_reuse_port:false) ---
envoy_pid=$(start_envoy "$CONF_DIR/bootstrap.yaml" 1)
if wait_ready 60; then
    check_eq "admin /ready (baseline)" "$(http_body http://127.0.0.1:9901/ready)" "LIVE"

    check_has "route prefix / -> backend_a" "$(http_body http://127.0.0.1:10000/)" "BACKEND=backend_a"

    ca=0; cb=0; i=0
    while [ "$i" -lt 18 ]; do
        r=$(http_body http://127.0.0.1:10000/api/x)
        case "$r" in *backend_a*) ca=$((ca+1)) ;; esac
        case "$r" in *backend_b*) cb=$((cb+1)) ;; esac
        i=$((i+1))
    done
    echo "  (weighted LB tally: backend_a=$ca backend_b=$cb)"
    check_ge "weighted LB backend_a hits" "$ca" "1"
    check_ge "weighted LB backend_b hits" "$cb" "1"
    TOTAL=$((TOTAL+1)); if [ "$ca" -gt "$cb" ]; then PASS=$((PASS+1)); echo "  PASS | weighted LB backend_a > backend_b"; else echo "  FAIL | weighted LB a=$ca not > b=$cb"; fi

    rw=$(http_body http://127.0.0.1:10000/api/foo/bar)
    check_has "path rewrite -> /cgi-bin/echo" "$rw" "/cgi-bin/echo"
    check_no  "path rewrite strips /api prefix" "$rw" "/api/"
    check_has "request header x-higress-added" "$rw" "X_HIGRESS_ADDED=starry"

    check_has "response header x-higress-gateway" "$(http_headers http://127.0.0.1:10000/api/x)" "x-higress-gateway: envoy-standalone"

    check_eq "retry route final status 503" "$(http_code http://127.0.0.1:10000/flaky)" "503"
    retries=$(http_body http://127.0.0.1:9901/stats | sed -n 's/^cluster.backend_fail.upstream_rq_retry: //p')
    check_ge "backend_fail upstream_rq_retry" "${retries:-0}" "1"

    c200=0; c429=0; i=0
    while [ "$i" -lt 5 ]; do
        code=$(http_code http://127.0.0.1:10000/limited)
        [ "$code" = "200" ] && c200=$((c200+1))
        [ "$code" = "429" ] && c429=$((c429+1))
        i=$((i+1))
    done
    echo "  (ratelimit tally: 200=$c200 429=$c429)"
    check_ge "local_ratelimit 200 allowed" "$c200" "1"
    check_ge "local_ratelimit 429 throttled" "$c429" "1"

    check_has "downstream TLS -> backend_a" "$(tls_body 127.0.0.1 10443 /)" "BACKEND=backend_a"
    check_eq  "upstream TLS route status" "$(http_code http://127.0.0.1:10000/secure)" "200"

    check_has "admin /stats server.state" "$(http_body http://127.0.0.1:9901/stats)" "server.state"
else
    echo "  FAIL | baseline Envoy did not become ready"
    tail -40 "$WORK/envoy-1.log" 2>/dev/null || true
fi
kill "$envoy_pid" 2>/dev/null || true
sleep 2

# --- reuse_port:true (exercises SO_REUSEPORT) ---
envoy_rp_pid=$(start_envoy "$CONF_DIR/bootstrap-reuseport.yaml" 2)
if wait_ready 60; then
    check_eq  "admin /ready (reuse_port:true)" "$(http_body http://127.0.0.1:9901/ready)" "LIVE"
    check_has "reuse_port listener serves traffic" "$(http_body http://127.0.0.1:10000/)" "BACKEND=backend_a"
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
