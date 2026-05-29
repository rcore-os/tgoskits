#!/bin/sh
set -eu

BASE=/tmp/nginx-phase60
CONF="$BASE/conf/log-fs.conf"
CONF_PREFIX="$BASE/conf/log-fs-prefix.conf"
WWW="$BASE/www"
OUT="$BASE/out"
LOGDIR="$BASE/logs"
TIMEOUT_CMD=

log() { printf 'NGINX_PHASE60_LOG: %s\n' "$*"; }
fail() { printf 'NGINX_PHASE60_TEST_FAILED\n'; log "$*"; exit 1; }

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
    printf 'phase60 log fs test\n' > "$WWW/index.html"
    cat > "$CONF" <<'EOF'
daemon off;
master_process off;
worker_processes 1;
error_log /tmp/nginx-phase60/logs/error.log debug;
pid /tmp/nginx-phase60/nginx.pid;
events { worker_connections 64; }
http {
    include /etc/nginx/mime.types;
    access_log /tmp/nginx-phase60/logs/access.log;
    server {
        listen 127.0.0.1:8080;
        root /tmp/nginx-phase60/www;
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
        run_with_timeout 1 curl -fsS -o /dev/null http://127.0.0.1:8080/ >/dev/null 2>&1 && return 0
        i=$((i + 1))
        sleep 1
    done
    return 1
}

test_access_log_line_growth() {
    before=$(wc -l < "$LOGDIR/access.log" 2>/dev/null || printf '0')
    run_with_timeout 5 curl -fsS -o /dev/null http://127.0.0.1:8080/
    run_with_timeout 5 curl -fsS -o /dev/null http://127.0.0.1:8080/
    after=$(wc -l < "$LOGDIR/access.log" 2>/dev/null || printf '0')
    [ "$after" -ge $((before + 2)) ]
}

test_error_log_writable() {
    test -f "$LOGDIR/error.log"
}

test_pid_file_present() {
    test -s "$BASE/nginx.pid"
}

test_reopen_logs() {
    run_with_timeout 5 nginx -s reopen -c "$CONF" -p "$BASE/"
    run_with_timeout 5 curl -fsS -o /dev/null http://127.0.0.1:8080/
    test -s "$LOGDIR/access.log"
}

test_pid_removed_after_stop() {
    run_with_timeout 5 nginx -s quit -c "$CONF" -p "$BASE/"
    i=0
    while [ "$i" -lt 6 ]; do
        [ ! -e "$BASE/nginx.pid" ] && return 0
        sleep 1
        i=$((i + 1))
    done
    return 1
}

prepare_prefix_conf() {
    mkdir -p "$BASE/prefix/conf" "$BASE/prefix/www"
    printf 'phase60 prefix test\n' > "$BASE/prefix/www/index.html"
    cat > "$CONF_PREFIX" <<'EOF'
daemon off;
master_process off;
worker_processes 1;
error_log logs/error.log debug;
pid run/nginx.pid;
events { worker_connections 64; }
http {
    include /etc/nginx/mime.types;
    access_log logs/access.log;
    server {
        listen 127.0.0.1:8081;
        root www;
        location / { index index.html; }
    }
}
EOF
}

test_prefix_relative_paths() {
    mkdir -p "$BASE/prefix/logs" "$BASE/prefix/run"
    nginx -t -c "$CONF_PREFIX" -p "$BASE/prefix/" || return 1
    nginx -c "$CONF_PREFIX" -p "$BASE/prefix/" > "$LOGDIR/nginx-prefix-stdout.log" 2>&1 &
    i=0
    while [ "$i" -lt 6 ]; do
        if run_with_timeout 1 curl -fsS -o "$OUT/prefix.body" http://127.0.0.1:8081/ >/dev/null 2>&1; then
            break
        fi
        i=$((i + 1))
        sleep 1
    done
    [ "$i" -lt 6 ] || return 1
    grep -qx 'phase60 prefix test' "$OUT/prefix.body"
    test -s "$BASE/prefix/logs/access.log"
    test -s "$BASE/prefix/run/nginx.pid"
    run_with_timeout 5 nginx -s quit -c "$CONF_PREFIX" -p "$BASE/prefix/"
    return 0
}

init_timeout_cmd
( sleep 90; log "watchdog timeout"; kill -TERM $$ ) &
prepare_packages || fail "prepare packages"
prepare_tree || fail "prepare tree"
start_nginx || fail "start nginx"
test_access_log_line_growth || fail "access log line growth"
test_error_log_writable || fail "error log writable"
test_pid_file_present || fail "pid file present"
test_reopen_logs || fail "reopen logs"
test_pid_removed_after_stop || fail "pid removed after stop"
prepare_prefix_conf || fail "prepare prefix conf"
test_prefix_relative_paths || fail "prefix relative paths"
cleanup_nginx
printf 'NGINX_PHASE60_TEST_PASSED\n'
