#!/bin/sh
set -eu

. /usr/bin/nginx-alpine-mirror.sh

BASE=/tmp/nginx-phase42
CONF="$BASE/conf/sendfile-on.conf"
WWW="$BASE/www"
OUT="$BASE/out"
LOGDIR="$BASE/logs"
TIMEOUT_CMD=

log() { printf 'NGINX_PHASE42_LOG: %s\n' "$*"; }
fail() { printf 'NGINX_PHASE42_TEST_FAILED\n'; log "$*"; exit 1; }

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
    nginx_apk_add_with_fallback nginx curl busybox-extras || return 1
}

prepare_tree() {
    rm -rf "$BASE"
    mkdir -p "$BASE/conf" "$WWW" "$OUT" "$LOGDIR"
    dd if=/dev/zero of="$WWW/large.bin" bs=1024 count=1024 >/dev/null 2>&1
    cat > "$CONF" <<'EOF'
daemon off;
master_process off;
worker_processes 1;
error_log /tmp/nginx-phase42/logs/error.log debug;
pid /tmp/nginx-phase42/nginx.pid;
events { worker_connections 64; }
http {
    include /etc/nginx/mime.types;
    access_log /tmp/nginx-phase42/logs/access.log;
    sendfile on;
    server {
        listen 127.0.0.1:8080;
        root /tmp/nginx-phase42/www;
    }
}
EOF
}

start_nginx() {
    nginx -t -c "$CONF" -p "$BASE/" || return 1
    nginx -c "$CONF" -p "$BASE/" > "$LOGDIR/nginx-stdout.log" 2>&1 &
    i=0
    while [ "$i" -lt 8 ]; do
        run_with_timeout 1 curl -fsS -o /dev/null http://127.0.0.1:8080/large.bin >/dev/null 2>&1 && return 0
        i=$((i + 1))
        sleep 1
    done
    return 1
}

test_sendfile_on_large_once() {
    run_with_timeout 10 curl -fsS -o "$OUT/large-once.bin" http://127.0.0.1:8080/large.bin
    [ "$(wc -c < "$OUT/large-once.bin")" -eq 1048576 ]
    cmp "$WWW/large.bin" "$OUT/large-once.bin"
}

test_sendfile_on_large_stability() {
    i=1
    while [ "$i" -le 5 ]; do
        run_with_timeout 10 curl -fsS -o "$OUT/large-$i.bin" http://127.0.0.1:8080/large.bin || return 1
        cmp "$WWW/large.bin" "$OUT/large-$i.bin" || return 1
        i=$((i + 1))
    done
}

init_timeout_cmd
( sleep 90; log "watchdog timeout"; kill -TERM $$ ) &
prepare_packages || fail "prepare packages"
prepare_tree || fail "prepare tree"
start_nginx || fail "start nginx"
test_sendfile_on_large_once || fail "sendfile on large file"
test_sendfile_on_large_stability || fail "sendfile on stability"
cleanup_nginx
printf 'NGINX_PHASE42_TEST_PASSED\n'
