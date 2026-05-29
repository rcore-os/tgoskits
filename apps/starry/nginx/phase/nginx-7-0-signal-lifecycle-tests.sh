#!/bin/sh
set -eu

BASE=/tmp/nginx-phase70
CONF="$BASE/conf/signal-lifecycle.conf"
WWW="$BASE/www"
OUT="$BASE/out"
LOGDIR="$BASE/logs"
TIMEOUT_CMD=
MASTER_PID=

log() { printf 'NGINX_PHASE70_LOG: %s\n' "$*"; }
fail() { printf 'NGINX_PHASE70_TEST_FAILED\n'; log "$*"; exit 1; }

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
    repo_file=/etc/apk/repositories
    original_repos="$(cat "$repo_file")"
    for mirror in https://mirrors.cernet.edu.cn/alpine https://dl-cdn.alpinelinux.org/alpine; do
        printf '%s\n' "$original_repos" | sed "s#http://[^/]*/alpine/#$mirror/#g;s#https://[^/]*/alpine/#$mirror/#g" > "$repo_file"
        rm -f /lib/apk/db/lock
        if run_with_timeout 40 apk --timeout 40 update && run_with_timeout 40 apk --timeout 40 add nginx curl busybox-extras; then return 0; fi
    done
    return 1
}

prepare_tree() {
    rm -rf "$BASE"
    mkdir -p "$BASE/conf" "$WWW" "$OUT" "$LOGDIR"
    printf 'phase70 signal lifecycle\n' > "$WWW/index.html"
    cat > "$CONF" <<'EOF'
daemon off;
master_process on;
worker_processes 1;
error_log /tmp/nginx-phase70/logs/error.log debug;
pid /tmp/nginx-phase70/nginx.pid;
events { worker_connections 64; }
http {
    include /etc/nginx/mime.types;
    access_log /tmp/nginx-phase70/logs/access.log;
    server {
        listen 127.0.0.1:8080;
        root /tmp/nginx-phase70/www;
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
        run_with_timeout 1 curl -fsS -o /dev/null http://127.0.0.1:8080/ >/dev/null 2>&1 && break
        i=$((i + 1))
        sleep 1
    done
    [ "$i" -lt 8 ] || return 1
    MASTER_PID=$(cat "$BASE/nginx.pid")
    [ -n "$MASTER_PID" ]
}

test_stop_fast_exit() {
    run_with_timeout 5 nginx -s stop -c "$CONF" -p "$BASE/"
    i=0
    while [ "$i" -lt 6 ]; do
        if ! kill -0 "$MASTER_PID" 2>/dev/null; then return 0; fi
        sleep 1
        i=$((i + 1))
    done
    return 1
}

restart_nginx() {
    start_nginx
}

test_reload_works() {
    run_with_timeout 5 nginx -s reload -c "$CONF" -p "$BASE/"
    sleep 1
    run_with_timeout 5 curl -fsS -o "$OUT/reload.body" http://127.0.0.1:8080/
    grep -qx 'phase70 signal lifecycle' "$OUT/reload.body"
}

test_reopen_works() {
    run_with_timeout 5 nginx -s reopen -c "$CONF" -p "$BASE/"
    run_with_timeout 5 curl -fsS -o /dev/null http://127.0.0.1:8080/
    test -s "$LOGDIR/access.log"
}

test_worker_kill_recover() {
    old_worker=$(ps | grep 'nginx: worker process' | grep -v grep | awk '{print $1}' | sed -n '1p')
    [ -n "$old_worker" ]
    kill -TERM "$old_worker"
    sleep 2
    new_worker=$(ps | grep 'nginx: worker process' | grep -v grep | awk '{print $1}' | sed -n '1p')
    [ -n "$new_worker" ]
    [ "$new_worker" != "$old_worker" ]
    kill -0 "$MASTER_PID"
    run_with_timeout 5 curl -fsS -o "$OUT/after-worker-kill.body" http://127.0.0.1:8080/
    grep -qx 'phase70 signal lifecycle' "$OUT/after-worker-kill.body"
}

init_timeout_cmd
( sleep 120; log "watchdog timeout"; kill -TERM $$ ) &
prepare_packages || fail "prepare packages"
prepare_tree || fail "prepare tree"
start_nginx || fail "start nginx"
test_stop_fast_exit || fail "nginx -s stop fast exit"
restart_nginx || fail "restart nginx"
test_reload_works || fail "nginx -s reload"
test_reopen_works || fail "nginx -s reopen"
test_worker_kill_recover || fail "worker kill recover"
cleanup_nginx
printf 'NGINX_PHASE70_TEST_PASSED\n'
