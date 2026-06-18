#!/bin/sh
set -eu

. /usr/bin/nginx-runner-lib.sh

BASE=/tmp/nginx-phase31
CONF="$BASE/conf/short-connection.conf"
WWW="$BASE/www"
OUT="$BASE/out"
LOGDIR="$BASE/logs"
TIMEOUT_CMD=
# x86_64 Starry currently has a high fixed cost for short-lived external
# commands (vfork/exec/exit/wait or user/kernel transition path), tracked in
# debug/ISSUE-005-x86-short-connection-timeout.md. Keep the HTTP assertion the
# same, but give each curl probe enough wall-clock budget for x86.
SHORT_CONN_CURL_TIMEOUT=15

log() { printf 'NGINX_PHASE31_LOG: %s\n' "$*"; }
fail() { printf 'NGINX_PHASE31_TEST_FAILED\n'; log "$*"; exit 1; }

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
    printf 'phase31 short connection file\n' > "$WWW/small.txt"
    cat > "$CONF" <<'EOF'
daemon off;
master_process off;
worker_processes 1;
error_log /tmp/nginx-phase31/logs/error.log debug;
pid /tmp/nginx-phase31/nginx.pid;
events { worker_connections 64; }
http { include /etc/nginx/mime.types; access_log /tmp/nginx-phase31/logs/access.log; sendfile off; keepalive_timeout 5; server { listen 127.0.0.1:8080; root /tmp/nginx-phase31/www; location / { index index.html; } } }
EOF
}

start_nginx() {
    nginx -t -c "$CONF" -p "$BASE/" || return 1
    nginx -c "$CONF" -p "$BASE/" > "$LOGDIR/nginx-stdout.log" 2>&1 &
    i=0
    while [ "$i" -lt 6 ]; do
        if run_with_timeout "$SHORT_CONN_CURL_TIMEOUT" curl -fsS -o /dev/null http://127.0.0.1:8080/small.txt >/dev/null 2>&1; then
            return 0
        fi
        i=$((i + 1))
        sleep 1
    done
    return 1
}

test_short_connection_100() {
    i=1
    while [ "$i" -le 100 ]; do
        run_with_timeout "$SHORT_CONN_CURL_TIMEOUT" curl -fsS -o /dev/null http://127.0.0.1:8080/small.txt >/dev/null 2>&1 || return 1
        i=$((i + 1))
    done
}

init_timeout_cmd
trap cleanup_nginx EXIT INT TERM
prepare_packages || fail "prepare packages"
prepare_tree || fail "prepare tree"
start_nginx || fail "start nginx"
test_short_connection_100 || fail "100 short connections"
cleanup_nginx
printf 'NGINX_PHASE31_TEST_PASSED\n'
