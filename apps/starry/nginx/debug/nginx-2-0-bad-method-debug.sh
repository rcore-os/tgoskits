#!/bin/sh
set -eu

BASE=/tmp/nginx-phase2-bad-method
CONF="$BASE/conf/http-basic.conf"
WWW="$BASE/www"
OUT="$BASE/out"
LOGDIR="$BASE/logs"
TIMEOUT_CMD=

log() { printf 'NGINX_BAD_METHOD_DEBUG_LOG: %s\n' "$*"; }
fail() { printf 'NGINX_BAD_METHOD_DEBUG_FAILED\n'; log "$*"; exit 1; }

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
    mkdir -p "$BASE/conf" "$WWW" "$LOGDIR" "$OUT"
    printf 'BAD_METHOD_DEBUG_OK\n' > "$WWW/index.html"
    cat > "$CONF" <<'EOF'
daemon off;
master_process off;
worker_processes 1;
error_log /tmp/nginx-phase2-bad-method/logs/error.log debug;
pid /tmp/nginx-phase2-bad-method/nginx.pid;
events { worker_connections 64; }
http { include /etc/nginx/mime.types; access_log /tmp/nginx-phase2-bad-method/logs/access.log; server { listen 127.0.0.1:8080; root /tmp/nginx-phase2-bad-method/www; location / { index index.html; } } }
EOF
}

start_nginx() {
    nginx -t -c "$CONF" -p "$BASE/" || return 1
    nginx -c "$CONF" -p "$BASE/" > "$LOGDIR/nginx-stdout.log" 2>&1 &
    i=0
    while [ "$i" -lt 6 ]; do
        run_with_timeout 1 curl -fsS -o /dev/null http://127.0.0.1:8080/ >/dev/null 2>&1 && return 0
        i=$((i + 1))
        sleep 1
    done
    return 1
}

probe_bad_method() {
    if command -v nc >/dev/null 2>&1; then
        NC='nc'
    elif busybox nc 2>&1 | grep -qi 'usage'; then
        NC='busybox nc'
    else
        fail "nc not available"
    fi
    code=$(run_with_timeout 3 curl -sS -o /dev/null -w '%{http_code}' -X BAD http://127.0.0.1:8080/ || true)
    [ -n "$code" ] || code='<empty>'
    log "curl_bad_method_status=$code"

    i=1
    while [ "$i" -le 5 ]; do
        out="$OUT/bad-$i.raw"
        { printf 'BAD / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n'; } | run_with_timeout 4 sh -c "$NC 127.0.0.1 8080" > "$out" || true
        tr -d '\r' < "$out" > "$OUT/bad-$i.norm"
        status=$(sed -n '1p' "$OUT/bad-$i.norm" || true)
        [ -n "$status" ] || status='<empty>'
        log "probe=$i status=$status"
        i=$((i + 1))
    done
    if grep -Eq '^HTTP/1.1 (400|405)' "$OUT"/bad-*.norm; then
        printf 'NGINX_BAD_METHOD_DEBUG_PASSED\n'
        return 0
    fi
    log "no 400/405 hit in 5 probes"
    return 1
}

init_timeout_cmd
( sleep 90; log "watchdog timeout"; kill -TERM $$ ) &
prepare_packages || fail "prepare packages"
prepare_tree || fail "prepare tree"
start_nginx || fail "start nginx"
probe_bad_method || {
    if [ -f "$LOGDIR/error.log" ]; then
        tail_line=$(sed -n '$p' "$LOGDIR/error.log" || true)
        [ -n "$tail_line" ] || tail_line='<empty>'
        log "error_log_tail=$tail_line"
    fi
    cleanup_nginx
    fail "bad method probes"
}
cleanup_nginx
