#!/bin/sh
set -eu

BASE=/tmp/nginx-multiworker
CONF="$BASE/conf/multi.conf"
LOGDIR="$BASE/logs"
OUT="$BASE/out"
TIMEOUT_CMD=
FAILURES=0

. /usr/bin/nginx-alpine-mirror.sh

log() { printf 'NGINX_MW_LOG: %s\n' "$*"; }
pass() { printf 'NGINX_MW_STEP_PASS: %s\n' "$*"; }
fail() { printf 'NGINX_MW_STEP_FAIL: %s\n' "$*"; FAILURES=$((FAILURES + 1)); }

run_step() {
    name=$1
    shift
    log "BEGIN $name"
    if "$@"; then
        pass "$name"
        return 0
    fi
    fail "$name"
    return 1
}

init_timeout_cmd() {
    if command -v timeout >/dev/null 2>&1; then
        TIMEOUT_CMD='timeout'
        return 0
    fi
    if busybox timeout 2>&1 | grep -qi 'usage'; then
        TIMEOUT_CMD='busybox timeout'
        return 0
    fi
    return 1
}

run_with_timeout() {
    sec=$1
    shift
    $TIMEOUT_CMD "$sec" "$@"
}

cleanup() {
    killall -q nginx 2>/dev/null || true
    sleep 1
    killall -q -9 nginx 2>/dev/null || true
}

finish() {
    status=$?
    cleanup
    if [ "$FAILURES" -eq 0 ] && [ "$status" -eq 0 ]; then
        printf 'NGINX_MW_TEST_PASSED\n'
        exit 0
    fi
    printf 'NGINX_MW_TEST_FAILED failures=%s status=%s\n' "$FAILURES" "$status"
    exit 1
}

trap finish EXIT

prepare_packages() {
    nginx_apk_add_with_fallback nginx curl busybox-extras procps || return 1
}

prepare_tree() {
    rm -rf "$BASE"
    mkdir -p "$BASE/conf" "$BASE/www" "$LOGDIR" "$OUT"
    printf 'MW_OK\n' > "$BASE/www/index.html"
    cat > "$CONF" <<'EOF'
daemon off;
master_process on;
worker_processes 2;
error_log /tmp/nginx-multiworker/logs/error.log debug;
pid /tmp/nginx-multiworker/nginx.pid;

events { worker_connections 128; }

http {
    include /etc/nginx/mime.types;
    access_log /tmp/nginx-multiworker/logs/access.log;
    aio off;
    keepalive_timeout 3;
    server {
        listen 127.0.0.1:8082;
        root /tmp/nginx-multiworker/www;
        location / { index index.html; }
    }
}
EOF
}

start_master2() {
    nginx -t -c "$CONF" -p "$BASE/" || return 1
    nginx -c "$CONF" -p "$BASE/" > "$LOGDIR/nginx.stdout" 2>&1 &
    i=0
    while [ "$i" -lt 6 ]; do
        if run_with_timeout 1 curl -fsS http://127.0.0.1:8082/ -o "$OUT/start.body" >/dev/null 2>&1; then
            return 0
        fi
        sleep 1
        i=$((i + 1))
    done
    log "startup curl timeout; nginx ps snapshot"
    ps -ef | grep nginx | grep -v grep || true
    log "error.log tail"
    tail -n 80 "$LOGDIR/error.log" || true
    return 1
}

probe_requests() {
    i=1
    while [ "$i" -le 8 ]; do
        run_with_timeout 1 curl -fsS http://127.0.0.1:8082/ -o "$OUT/req-$i.body" >/dev/null 2>&1 || return 1
        i=$((i + 1))
    done
    return 0
}

probe_workers() {
    ps -ef > "$OUT/ps.snapshot"
    workers=$(grep 'nginx: worker process' "$OUT/ps.snapshot" | grep -v grep | wc -l)
    log "worker_count=$workers"
    log "ps snapshot"
    cat "$OUT/ps.snapshot"
    return 0
}

quit_and_reap() {
    run_with_timeout 2 nginx -s quit -c "$CONF" -p "$BASE/" >/dev/null 2>&1 || return 1
    i=0
    while [ "$i" -lt 4 ]; do
        left=$(ps -ef | grep 'nginx: worker process' | grep -v grep | wc -l)
        [ "$left" -eq 0 ] && return 0
        sleep 1
        i=$((i + 1))
    done
    return 1
}

run_step "init timeout helper" init_timeout_cmd || exit 1
run_step "prepare packages" prepare_packages || exit 1
run_step "prepare files" prepare_tree || exit 1
run_step "start master+2worker" start_master2 || exit 1
run_step "observe worker count" probe_workers || exit 1
run_step "8 quick requests" probe_requests || exit 1
run_step "quit and no worker residue" quit_and_reap || exit 1
