#!/bin/sh
set -eu

. /usr/bin/nginx-runner-lib.sh

BASE=/tmp/nginx-phase50
CONF="$BASE/conf/request-body.conf"
WWW="$BASE/www"
OUT="$BASE/out"
LOGDIR="$BASE/logs"
TIMEOUT_CMD=

log() { printf 'NGINX_PHASE50_LOG: %s\n' "$*"; }
fail() { printf 'NGINX_PHASE50_TEST_FAILED\n'; log "$*"; exit 1; }

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
    mkdir -p "$BASE/conf" "$WWW" "$OUT" "$LOGDIR" "$BASE/client_temp"
    printf 'phase50 body test file\n' > "$WWW/index.html"
    cat > "$CONF" <<'EOF'
daemon off;
master_process off;
worker_processes 1;
error_log /tmp/nginx-phase50/logs/error.log debug;
pid /tmp/nginx-phase50/nginx.pid;
events { worker_connections 64; }
http {
    include /etc/nginx/mime.types;
    access_log /tmp/nginx-phase50/logs/access.log;
    client_max_body_size 4k;
    client_body_buffer_size 1k;
    client_body_temp_path /tmp/nginx-phase50/client_temp;
    server {
        listen 127.0.0.1:8080;
        root /tmp/nginx-phase50/www;
        location / { index index.html; }
    }
}
EOF
}

start_nginx() {
    nginx -t -c "$CONF" -p "$BASE/" || return 1
    nginx -c "$CONF" -p "$BASE/" > "$LOGDIR/nginx-stdout.log" 2>&1 &
    i=0
    while [ "$i" -lt 8 ]; do
        run_with_timeout 5 curl -fsS -o /dev/null http://127.0.0.1:8080/ >/dev/null 2>&1 && return 0
        i=$((i + 1))
        sleep 1
    done
    return 1
}

test_post_small() {
    code=$(run_with_timeout 8 curl -sS -o "$OUT/post-small.body" -w '%{http_code}' -X POST --data 'abc' http://127.0.0.1:8080/ || true)
    [ "$code" = "405" ] || [ "$code" = "404" ] || [ "$code" = "200" ]
}

test_post_over_buffer_path() {
    dd if=/dev/zero of="$OUT/post-over-buffer.bin" bs=1024 count=2 >/dev/null 2>&1
    code=$(run_with_timeout 12 curl -sS -o "$OUT/post-over-buffer.body" -w '%{http_code}' -X POST --data-binary "@$OUT/post-over-buffer.bin" http://127.0.0.1:8080/ || true)
    [ "$code" = "405" ] || [ "$code" = "404" ] || [ "$code" = "200" ] || [ "$code" = "413" ]
}

test_post_too_large_413() {
    dd if=/dev/zero of="$OUT/post-too-large.bin" bs=1024 count=8 >/dev/null 2>&1
    code=$(run_with_timeout 12 curl -sS -o "$OUT/post-too-large.body" -w '%{http_code}' -X POST --data-binary "@$OUT/post-too-large.bin" http://127.0.0.1:8080/ || true)
    [ "$code" = "413" ]
}

init_timeout_cmd
trap cleanup_nginx EXIT INT TERM
prepare_packages || fail "prepare packages"
prepare_tree || fail "prepare tree"
start_nginx || fail "start nginx"
test_post_small || fail "small POST"
test_post_over_buffer_path || fail "over buffer POST path"
test_post_too_large_413 || fail "too large POST 413"
cleanup_nginx
printf 'NGINX_PHASE50_TEST_PASSED\n'
