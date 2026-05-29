#!/bin/sh
set -eu

BASE=/tmp/nginx-phase32
CONF="$BASE/conf/keepalive.conf"
WWW="$BASE/www"
OUT="$BASE/out"
LOGDIR="$BASE/logs"
TIMEOUT_CMD=
NC_CMD=

log() { printf 'NGINX_PHASE32_LOG: %s\n' "$*"; }
fail() { printf 'NGINX_PHASE32_TEST_FAILED\n'; log "$*"; exit 1; }

init_timeout_cmd() {
    if command -v timeout >/dev/null 2>&1; then
        TIMEOUT_CMD='timeout'
        return
    fi
    if busybox timeout 2>&1 | grep -qi 'usage'; then
        TIMEOUT_CMD='busybox timeout'
        return
    fi
    fail "timeout command not available"
}

run_with_timeout() {
    sec=$1
    shift
    $TIMEOUT_CMD "$sec" "$@"
}

init_nc_cmd() {
    if command -v nc.openbsd >/dev/null 2>&1; then
        NC_CMD='nc.openbsd -w 2'
        return
    fi
    if command -v nc >/dev/null 2>&1; then
        NC_CMD='nc -w 2'
        return
    fi
    if busybox nc 2>&1 | grep -qi 'usage'; then
        NC_CMD='busybox nc -w 2'
        return
    fi
    fail "nc command not available"
}

cleanup_nginx() {
    killall -q nginx 2>/dev/null || true
    sleep 1
    killall -q -9 nginx 2>/dev/null || true
}

prepare_packages() {
    repo_file=/etc/apk/repositories
    original_repos="$(cat "$repo_file")"
    for mirror in https://mirrors.cernet.edu.cn/alpine https://dl-cdn.alpinelinux.org/alpine; do
        printf '%s\n' "$original_repos" | sed "s#http://[^/]*/alpine/#$mirror/#g;s#https://[^/]*/alpine/#$mirror/#g" > "$repo_file"
        rm -f /lib/apk/db/lock
        if run_with_timeout 40 apk --timeout 40 update \
            && run_with_timeout 40 apk --timeout 40 add nginx curl busybox-extras netcat-openbsd; then
            return 0
        fi
    done
    return 1
}

prepare_tree() {
    rm -rf "$BASE"
    mkdir -p "$BASE/conf" "$WWW" "$OUT" "$LOGDIR"
    printf 'phase32 keepalive file\n' > "$WWW/small.txt"
    cat > "$CONF" <<'EOF'
daemon off;
master_process off;
worker_processes 1;
error_log /tmp/nginx-phase32/logs/error.log debug;
pid /tmp/nginx-phase32/nginx.pid;
events { worker_connections 64; }
http {
    include /etc/nginx/mime.types;
    access_log /tmp/nginx-phase32/logs/access.log;
    sendfile off;
    keepalive_timeout 5;
    server {
        listen 127.0.0.1:8080;
        root /tmp/nginx-phase32/www;
        location / { index index.html; }
    }
}
EOF
}

start_nginx() {
    nginx -t -c "$CONF" -p "$BASE/" || return 1
    nginx -c "$CONF" -p "$BASE/" > "$LOGDIR/nginx-stdout.log" 2>&1 &
    i=0
    while [ "$i" -lt 6 ]; do
        if run_with_timeout 1 curl -fsS -o /dev/null http://127.0.0.1:8080/small.txt >/dev/null 2>&1; then
            return 0
        fi
        i=$((i + 1))
        sleep 1
    done
    return 1
}

test_keepalive_two_requests() {
    run_with_timeout 10 sh -c "{ \
        printf 'GET /small.txt HTTP/1.1\\r\\nHost: localhost\\r\\nConnection: keep-alive\\r\\n\\r\\n'; \
        printf 'GET /small.txt HTTP/1.1\\r\\nHost: localhost\\r\\nConnection: close\\r\\n\\r\\n'; \
    } | $NC_CMD 127.0.0.1 8080" > "$OUT/keepalive-two.raw"
    tr -d '\r' < "$OUT/keepalive-two.raw" > "$OUT/keepalive-two.norm"
    count=$(grep -c '^HTTP/1.1 200' "$OUT/keepalive-two.norm" || true)
    [ "$count" -eq 2 ]
}

test_connection_close_behavior() {
    run_with_timeout 10 sh -c "{ \
        printf 'GET /small.txt HTTP/1.1\\r\\nHost: localhost\\r\\nConnection: close\\r\\n\\r\\n'; \
        printf 'GET /small.txt HTTP/1.1\\r\\nHost: localhost\\r\\nConnection: keep-alive\\r\\n\\r\\n'; \
    } | $NC_CMD 127.0.0.1 8080" > "$OUT/conn-close.raw" || true
    tr -d '\r' < "$OUT/conn-close.raw" > "$OUT/conn-close.norm"
    count=$(grep -c '^HTTP/1.1 200' "$OUT/conn-close.norm" || true)
    [ "$count" -eq 1 ]
}

test_idle_timeout_close() {
    run_with_timeout 20 sh -c "{ \
        printf 'GET /small.txt HTTP/1.1\\r\\nHost: localhost\\r\\nConnection: keep-alive\\r\\n\\r\\n'; \
        sleep 7; \
        printf 'GET /small.txt HTTP/1.1\\r\\nHost: localhost\\r\\nConnection: close\\r\\n\\r\\n'; \
    } | $NC_CMD 127.0.0.1 8080" > "$OUT/idle-timeout.raw" || true
    tr -d '\r' < "$OUT/idle-timeout.raw" > "$OUT/idle-timeout.norm"
    count=$(grep -c '^HTTP/1.1 200' "$OUT/idle-timeout.norm" || true)
    [ "$count" -eq 1 ]
}

init_timeout_cmd
init_nc_cmd
( sleep 90; log "watchdog timeout"; kill -TERM $$ ) &
prepare_packages || fail "prepare packages"
prepare_tree || fail "prepare tree"
start_nginx || fail "start nginx"
test_keepalive_two_requests || fail "same connection two requests keepalive"
test_connection_close_behavior || fail "Connection: close behavior"
test_idle_timeout_close || fail "idle timeout closes keepalive connection"
cleanup_nginx
printf 'NGINX_PHASE32_TEST_PASSED\n'
