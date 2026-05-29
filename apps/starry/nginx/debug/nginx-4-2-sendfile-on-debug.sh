#!/bin/sh
set -eu

BASE=/tmp/nginx-phase42-debug
CONF="$BASE/conf/sendfile-on.conf"
WWW="$BASE/www"
OUT="$BASE/out"
LOGDIR="$BASE/logs"
TIMEOUT_CMD=

log() { printf 'NGINX_PHASE42_DEBUG_LOG: %s\n' "$*"; }
fail() { printf 'NGINX_PHASE42_DEBUG_FAILED\n'; log "$*"; exit 1; }

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
    dd if=/dev/zero of="$WWW/large.bin" bs=1024 count=1024 >/dev/null 2>&1
    cat > "$CONF" <<'EOF'
daemon off;
master_process off;
worker_processes 1;
error_log /tmp/nginx-phase42-debug/logs/error.log debug;
pid /tmp/nginx-phase42-debug/nginx.pid;
events { worker_connections 64; }
http {
    include /etc/nginx/mime.types;
    access_log /tmp/nginx-phase42-debug/logs/access.log;
    sendfile on;
    keepalive_timeout 5;
    server {
        listen 127.0.0.1:8080;
        root /tmp/nginx-phase42-debug/www;
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

probe_sendfile_on() {
    i=1
    while [ "$i" -le 5 ]; do
        out="$OUT/large-$i.bin"
        hdr="$OUT/large-$i.h"
        if run_with_timeout 15 curl -sS -D "$hdr" -o "$out" http://127.0.0.1:8080/large.bin; then
            size=$(wc -c < "$out")
            log "probe=$i curl_ok size=$size"
        else
            size=$(wc -c < "$out" 2>/dev/null || printf '0')
            log "probe=$i curl_fail size=$size"
        fi
        i=$((i + 1))
    done

    if [ -f "$LOGDIR/error.log" ]; then
        tail_line=$(sed -n '$p' "$LOGDIR/error.log" || true)
        [ -n "$tail_line" ] || tail_line='<empty>'
        log "error_log_tail=$tail_line"
    fi

    printf 'NGINX_PHASE42_DEBUG_DONE\n'
}

init_timeout_cmd
( sleep 120; log "watchdog timeout"; kill -TERM $$ ) &
prepare_packages || fail "prepare packages"
prepare_tree || fail "prepare tree"
start_nginx || fail "start nginx"
probe_sendfile_on
cleanup_nginx
