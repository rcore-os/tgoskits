#!/bin/sh
set -eu

BASE=/tmp/nginx-phase12
CONF_DIR="$BASE/conf"
LOG_DIR="$BASE/logs"
OUT="$BASE/out"
WWW="$BASE/www"
TIMEOUT_CMD=

log() { printf 'NGINX_PHASE12_LOG: %s\n' "$*"; }
fail() { printf 'NGINX_PHASE12_TEST_FAILED\n'; log "$*"; exit 1; }

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
        if run_with_timeout 40 apk --timeout 40 update && run_with_timeout 40 apk --timeout 40 add nginx curl busybox-extras procps; then return 0; fi
    done
    return 1
}

prepare_files() {
    rm -rf "$BASE"
    mkdir -p "$CONF_DIR" "$LOG_DIR" "$OUT" "$WWW"
    printf 'PHASE12_OK\n' > "$WWW/index.html"
    cat > "$CONF_DIR/master1.conf" <<'EOF'
daemon off;
master_process on;
worker_processes 1;
error_log /tmp/nginx-phase12/logs/error-master1.log debug;
pid /tmp/nginx-phase12/nginx-master1.pid;
events { worker_connections 64; }
http { include /etc/nginx/mime.types; access_log /tmp/nginx-phase12/logs/access-master1.log; server { listen 127.0.0.1:8081; root /tmp/nginx-phase12/www; location / { index index.html; } } }
EOF
}

wait_http_ok() {
    url=$1
    i=0
    while [ "$i" -lt 6 ]; do
        if run_with_timeout 1 curl -fsS "$url" -o "$OUT/http.body" >/dev/null 2>&1; then return 0; fi
        i=$((i + 1))
        sleep 1
    done
    return 1
}

count_nginx_procs() {
    if command -v pgrep >/dev/null 2>&1; then
        pgrep -xc nginx
        return
    fi
    ps | grep '/usr/sbin/nginx\| nginx$' | grep -v grep | wc -l
}

assert_no_worker_or_zombie() {
    if ps -ef | grep -E 'nginx: worker process|nginx: master process' | grep -v grep >/dev/null 2>&1; then
        return 1
    fi
    if ps -ef | grep ' Z ' | grep -E 'nginx( |$)' | grep -v grep >/dev/null 2>&1; then
        return 1
    fi
    return 0
}

test_master1_lifecycle() {
    nginx -t -c "$CONF_DIR/master1.conf" -p "$BASE/" || return 1
    nginx -c "$CONF_DIR/master1.conf" -p "$BASE/" > "$LOG_DIR/master1.stdout" 2>&1 &
    wait_http_ok http://127.0.0.1:8081/ || return 1

    procs=$(count_nginx_procs)
    log "phase1.2 nginx_proc_count_before_reload=$procs"
    [ "$procs" -ge 2 ] || return 1

    run_with_timeout 2 nginx -s reload -c "$CONF_DIR/master1.conf" -p "$BASE/" >/dev/null 2>&1 || return 1
    wait_http_ok http://127.0.0.1:8081/ || return 1

    procs=$(count_nginx_procs)
    log "phase1.2 nginx_proc_count_after_reload=$procs"
    [ "$procs" -ge 2 ] || return 1

    run_with_timeout 2 nginx -s quit -c "$CONF_DIR/master1.conf" -p "$BASE/" >/dev/null 2>&1 || return 1

    i=0
    while [ "$i" -lt 6 ]; do
        if assert_no_worker_or_zombie; then
            return 0
        fi
        i=$((i + 1))
        sleep 1
    done
    return 1
}

init_timeout_cmd
( sleep 90; log "watchdog timeout"; kill -TERM $$ ) &
prepare_packages || fail "prepare packages"
prepare_files || fail "prepare files"
test_master1_lifecycle || fail "phase1.2"
cleanup_nginx
printf 'NGINX_PHASE12_TEST_PASSED\n'
