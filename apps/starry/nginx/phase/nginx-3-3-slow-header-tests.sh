#!/bin/sh
set -eu

. /usr/bin/nginx-alpine-mirror.sh

BASE=/tmp/nginx-phase33
CONF="$BASE/conf/slow-header.conf"
WWW="$BASE/www"
OUT="$BASE/out"
LOGDIR="$BASE/logs"
TIMEOUT_CMD=
NC_CMD=

log() { printf 'NGINX_PHASE33_LOG: %s\n' "$*"; }
fail() { printf 'NGINX_PHASE33_TEST_FAILED\n'; log "$*"; exit 1; }

init_timeout_cmd() {
    if command -v timeout >/dev/null 2>&1; then TIMEOUT_CMD='timeout'; return; fi
    if busybox timeout 2>&1 | grep -qi 'usage'; then TIMEOUT_CMD='busybox timeout'; return; fi
    fail "timeout command not available"
}

run_with_timeout() { sec=$1; shift; $TIMEOUT_CMD "$sec" "$@"; }

init_nc_cmd() {
    if command -v nc >/dev/null 2>&1; then NC_CMD='nc -w 2'; return; fi
    if command -v nc.openbsd >/dev/null 2>&1; then NC_CMD='nc.openbsd -w 2'; return; fi
    if busybox nc 2>&1 | grep -qi 'usage'; then NC_CMD='busybox nc -w 2'; return; fi
    fail "nc command not available"
}

cleanup_nginx() {
    killall -q nginx 2>/dev/null || true
    sleep 1
    killall -q -9 nginx 2>/dev/null || true
}

prepare_packages() {
    nginx_apk_add_with_fallback nginx curl busybox-extras netcat-openbsd || return 1
}

prepare_tree() {
    rm -rf "$BASE"
    mkdir -p "$BASE/conf" "$WWW" "$OUT" "$LOGDIR"
    printf 'phase33 slow header file\n' > "$WWW/small.txt"
    cat > "$CONF" <<'EOF'
daemon off;
master_process off;
worker_processes 1;
error_log /tmp/nginx-phase33/logs/error.log debug;
pid /tmp/nginx-phase33/nginx.pid;
events { worker_connections 64; }
http {
    include /etc/nginx/mime.types;
    access_log /tmp/nginx-phase33/logs/access.log;
    sendfile off;
    keepalive_timeout 5;
    client_header_timeout 4;
    server {
        listen 127.0.0.1:8080;
        root /tmp/nginx-phase33/www;
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
        run_with_timeout 1 curl -fsS -o /dev/null http://127.0.0.1:8080/small.txt >/dev/null 2>&1 && return 0
        i=$((i + 1))
        sleep 1
    done
    return 1
}

test_slow_header_within_timeout() {
    run_with_timeout 12 sh -c "{ \
        printf 'GET /small.txt HTTP/1.1\\r\\n'; \
        sleep 1; \
        printf 'Host: localhost\\r\\n'; \
        sleep 1; \
        printf 'Connection: close\\r\\n\\r\\n'; \
    } | $NC_CMD 127.0.0.1 8080" > "$OUT/slow-ok.raw"
    tr -d '\r' < "$OUT/slow-ok.raw" > "$OUT/slow-ok.norm"
    grep -Eq '^HTTP/1.1 200' "$OUT/slow-ok.norm"
}

test_slow_header_timeout_close() {
    run_with_timeout 12 sh -c "{ \
        printf 'GET /small.txt HTTP/1.1\\r\\n'; \
        sleep 6; \
        printf 'Host: localhost\\r\\nConnection: close\\r\\n\\r\\n'; \
    } | $NC_CMD 127.0.0.1 8080" > "$OUT/slow-timeout.raw" || true
    tr -d '\r' < "$OUT/slow-timeout.raw" > "$OUT/slow-timeout.norm"
    ! grep -Eq '^HTTP/1.1 200' "$OUT/slow-timeout.norm"
}

test_slow_header_not_block_other_conn() {
    run_with_timeout 15 sh -c "{ \
        { printf 'GET /small.txt HTTP/1.1\\r\\n'; sleep 6; printf 'Host: localhost\\r\\nConnection: close\\r\\n\\r\\n'; } \
            | $NC_CMD 127.0.0.1 8080 >/dev/null 2>&1 & \
        sleep 1; \
        curl -fsS -o '$OUT/parallel.body' http://127.0.0.1:8080/small.txt >/dev/null; \
        wait; \
    }"
    grep -qx 'phase33 slow header file' "$OUT/parallel.body"
}

init_timeout_cmd
init_nc_cmd
( sleep 90; log "watchdog timeout"; kill -TERM $$ ) &
prepare_packages || fail "prepare packages"
prepare_tree || fail "prepare tree"
start_nginx || fail "start nginx"
test_slow_header_within_timeout || fail "slow header under timeout"
test_slow_header_timeout_close || fail "slow header timeout close"
test_slow_header_not_block_other_conn || fail "slow header should not block others"
cleanup_nginx
printf 'NGINX_PHASE33_TEST_PASSED\n'
