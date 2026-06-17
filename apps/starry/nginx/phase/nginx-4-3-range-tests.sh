#!/bin/sh
set -eu

. /usr/bin/nginx-runner-lib.sh

BASE=/tmp/nginx-phase43
CONF="$BASE/conf/range.conf"
WWW="$BASE/www"
OUT="$BASE/out"
LOGDIR="$BASE/logs"
TIMEOUT_CMD=

log() { printf 'NGINX_PHASE43_LOG: %s\n' "$*"; }
fail() { printf 'NGINX_PHASE43_TEST_FAILED\n'; log "$*"; exit 1; }

init_timeout_cmd() {
    if command -v timeout >/dev/null 2>&1; then TIMEOUT_CMD='timeout'; return; fi
    if busybox timeout 2>&1 | grep -qi 'usage'; then TIMEOUT_CMD='busybox timeout'; return; fi
    fail "timeout command not available"
}

run_with_timeout() { sec=$1; shift; $TIMEOUT_CMD "$sec" "$@"; }

cleanup_nginx() {
    killall -q nginx 2>/dev/null || true
    sleep 1
    killall -q -9 nginx 2>/dev/null || true
}

prepare_packages() {
    runner_ensure_packages || return 1
}

prepare_tree() {
    rm -rf "$BASE"
    mkdir -p "$BASE/conf" "$WWW" "$OUT" "$LOGDIR"
    dd if=/dev/zero of="$WWW/large.bin" bs=1024 count=1024 >/dev/null 2>&1
    cat > "$CONF" <<'EOF'
daemon off;
master_process off;
worker_processes 1;
error_log /tmp/nginx-phase43/logs/error.log debug;
pid /tmp/nginx-phase43/nginx.pid;
events { worker_connections 64; }
http {
    include /etc/nginx/mime.types;
    access_log /tmp/nginx-phase43/logs/access.log;
    sendfile on;
    server {
        listen 127.0.0.1:8080;
        root /tmp/nginx-phase43/www;
    }
}
EOF
}

start_nginx() {
    nginx -t -c "$CONF" -p "$BASE/" || return 1
    nginx -c "$CONF" -p "$BASE/" > "$LOGDIR/nginx-stdout.log" 2>&1 &
    i=0
    while [ "$i" -lt 8 ]; do
        run_with_timeout 5 curl -fsS -o /dev/null http://127.0.0.1:8080/large.bin >/dev/null 2>&1 && return 0
        i=$((i + 1))
        sleep 1
    done
    return 1
}

test_range_0_15() {
    run_with_timeout 10 curl -fsS -D "$OUT/r0-15.h" -H 'Range: bytes=0-15' -o "$OUT/r0-15.bin" http://127.0.0.1:8080/large.bin
    [ "$(wc -c < "$OUT/r0-15.bin")" -eq 16 ]
    grep -qi '^HTTP/1.1 206' "$OUT/r0-15.h"
    grep -qi '^Content-Range: bytes 0-15/1048576' "$OUT/r0-15.h"
}

test_range_100_199() {
    run_with_timeout 10 curl -fsS -D "$OUT/r100-199.h" -H 'Range: bytes=100-199' -o "$OUT/r100-199.bin" http://127.0.0.1:8080/large.bin
    [ "$(wc -c < "$OUT/r100-199.bin")" -eq 100 ]
    grep -qi '^HTTP/1.1 206' "$OUT/r100-199.h"
    grep -qi '^Content-Range: bytes 100-199/1048576' "$OUT/r100-199.h"
}

test_range_suffix_64() {
    run_with_timeout 10 curl -fsS -D "$OUT/r-64.h" -H 'Range: bytes=-64' -o "$OUT/r-64.bin" http://127.0.0.1:8080/large.bin
    [ "$(wc -c < "$OUT/r-64.bin")" -eq 64 ]
    grep -qi '^HTTP/1.1 206' "$OUT/r-64.h"
    grep -qi '^Content-Range: bytes 1048512-1048575/1048576' "$OUT/r-64.h"
}

init_timeout_cmd
trap cleanup_nginx EXIT INT TERM
prepare_packages || fail "prepare packages"
prepare_tree || fail "prepare tree"
start_nginx || fail "start nginx"
test_range_0_15 || fail "range bytes=0-15"
test_range_100_199 || fail "range bytes=100-199"
test_range_suffix_64 || fail "range bytes=-64"
cleanup_nginx
printf 'NGINX_PHASE43_TEST_PASSED\n'
