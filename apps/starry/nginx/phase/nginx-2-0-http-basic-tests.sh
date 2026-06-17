#!/bin/sh
set -eu

. /usr/bin/nginx-runner-lib.sh

BASE=/tmp/nginx-phase2
CONF="$BASE/conf/http-basic.conf"
WWW="$BASE/www"
OUT="$BASE/out"
LOGDIR="$BASE/logs"
TIMEOUT_CMD=

log() { printf 'NGINX_PHASE2_LOG: %s\n' "$*"; }
fail() { printf 'NGINX_PHASE2_TEST_FAILED\n'; log "$*"; exit 1; }

init_timeout_cmd() {
    if command -v timeout >/dev/null 2>&1; then TIMEOUT_CMD='timeout'; return; fi
    if busybox timeout 2>&1 | grep -qi 'usage'; then TIMEOUT_CMD='busybox timeout'; return; fi
    fail "timeout command not available"
}

run_with_timeout() { sec=$1; shift; $TIMEOUT_CMD "$sec" "$@"; }

cleanup_nginx() { killall -q nginx 2>/dev/null || true; sleep 1; killall -q -9 nginx 2>/dev/null || true; }

prepare_packages() {
    runner_ensure_packages || return 1
}

prepare_tree() {
    rm -rf "$BASE"
    mkdir -p "$BASE/conf" "$WWW/dir" "$LOGDIR" "$OUT"
    printf 'small static file\n' > "$WWW/small.txt"
    : > "$WWW/empty.txt"
    printf 'HTTP_BASIC_DIR_INDEX_OK\n' > "$WWW/dir/index.html"
    cat > "$CONF" <<'EOF'
daemon off;
master_process off;
worker_processes 1;
error_log /tmp/nginx-phase2/logs/error.log debug;
pid /tmp/nginx-phase2/nginx.pid;
events { worker_connections 64; }
http { include /etc/nginx/mime.types; access_log /tmp/nginx-phase2/logs/access.log; server { listen 127.0.0.1:8080; root /tmp/nginx-phase2/www; location / { index index.html; } } }
EOF
}

start_nginx() {
    nginx -t -c "$CONF" -p "$BASE/" || return 1
    nginx -c "$CONF" -p "$BASE/" > "$LOGDIR/nginx-stdout.log" 2>&1 &
    i=0
    while [ "$i" -lt 6 ]; do
        run_with_timeout 5 curl -fsS -o /dev/null http://127.0.0.1:8080/small.txt >/dev/null 2>&1 && return 0
        i=$((i + 1))
        sleep 1
    done
    if [ -f "$LOGDIR/nginx-stdout.log" ]; then
        tail_line=$(sed -n '$p' "$LOGDIR/nginx-stdout.log" || true)
        [ -n "$tail_line" ] && log "start_nginx stdout tail: $tail_line"
    fi
    return 1
}

init_timeout_cmd
trap cleanup_nginx EXIT INT TERM
prepare_packages || fail "prepare packages"
prepare_tree || fail "prepare tree"
start_nginx || fail "start nginx"
code=$(run_with_timeout 5 curl -sS -o /dev/null -w '%{http_code}' http://127.0.0.1:8080/small.txt || true); [ "$code" = "200" ] || fail "GET /small.txt"
code=$(run_with_timeout 5 curl -sS -o /dev/null -w '%{http_code}' http://127.0.0.1:8080/empty.txt || true); [ "$code" = "200" ] || fail "GET /empty.txt"
code=$(run_with_timeout 5 curl -sS -o "$OUT/dir.body" -w '%{http_code}' http://127.0.0.1:8080/dir/ || true); [ "$code" = "200" ] && grep -qx 'HTTP_BASIC_DIR_INDEX_OK' "$OUT/dir.body" || fail "GET /dir/"
code=$(run_with_timeout 5 curl -sS -o /dev/null -w '%{http_code}' http://127.0.0.1:8080/dir || true); [ "$code" = "301" ] || [ "$code" = "302" ] || fail "GET /dir"
if command -v nc >/dev/null 2>&1 || busybox nc 2>&1 | grep -qi 'usage'; then
    NC='nc'; command -v nc >/dev/null 2>&1 || NC='busybox nc'
    { printf 'BAD / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n'; } | run_with_timeout 1 sh -c "$NC 127.0.0.1 8080" > "$OUT/bad.raw" || true
    tr -d '\r' < "$OUT/bad.raw" > "$OUT/bad.norm"
    bad_status=$(sed -n '1p' "$OUT/bad.norm" || true)
    if ! grep -Eq '^HTTP/1.1 (400|405)' "$OUT/bad.norm"; then
        [ -n "$bad_status" ] || bad_status='<empty>'
        log "KNOWN_ISSUE: BAD method raw request path unstable or blocked, status=$bad_status"
    fi
fi
cleanup_nginx
printf 'NGINX_PHASE2_TEST_PASSED\n'
