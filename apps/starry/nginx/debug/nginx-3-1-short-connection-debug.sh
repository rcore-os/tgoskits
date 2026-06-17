#!/bin/sh
set -eu

. /usr/bin/nginx-runner-lib.sh

BASE=/tmp/nginx-phase31-debug
CONF="$BASE/conf/short-connection.conf"
WWW="$BASE/www"
OUT="$BASE/out"
LOGDIR="$BASE/logs"
TIMEOUT_CMD=
ITERATIONS="${NGINX_SHORT_CONN_DEBUG_ITERATIONS:-120}"
USE_TIMEOUT="${NGINX_SHORT_CONN_DEBUG_USE_TIMEOUT:-1}"

log() { printf 'NGINX_SHORT_CONN_DEBUG_LOG: %s\n' "$*"; }
fail() { printf 'NGINX_SHORT_CONN_DEBUG_FAILED\n'; log "$*"; exit 1; }

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

run_curl_probe() {
    sec=$1
    shift
    if [ "$USE_TIMEOUT" = "0" ]; then
        "$@"
    else
        run_with_timeout "$sec" "$@"
    fi
}

cleanup_nginx() {
    killall -q nginx 2>/dev/null || true
    sleep 1
    killall -q -9 nginx 2>/dev/null || true
}

prepare_tree() {
    rm -rf "$BASE"
    mkdir -p "$BASE/conf" "$WWW" "$OUT" "$LOGDIR"
    printf 'phase31 short connection debug file\n' > "$WWW/small.txt"
    cat > "$CONF" <<'EOF'
daemon off;
master_process off;
worker_processes 1;
error_log /tmp/nginx-phase31-debug/logs/error.log debug;
pid /tmp/nginx-phase31-debug/nginx.pid;
events { worker_connections 64; }
http { include /etc/nginx/mime.types; access_log /tmp/nginx-phase31-debug/logs/access.log; sendfile off; keepalive_timeout 5; server { listen 127.0.0.1:8080; root /tmp/nginx-phase31-debug/www; location / { index index.html; } } }
EOF
}

dump_state() {
    log "nginx_pids=$(pgrep nginx 2>/dev/null | tr '\n' ' ' || true)"
    if command -v ss >/dev/null 2>&1; then
        ss -tan 2>/dev/null | grep '127.0.0.1:8080' || true
    elif command -v netstat >/dev/null 2>&1; then
        netstat -tan 2>/dev/null | grep '127.0.0.1:8080' || true
    fi
    if [ -f "$LOGDIR/access.log" ]; then
        log "access_count=$(wc -l < "$LOGDIR/access.log")"
        log "access_tail_begin"
        tail -n 8 "$LOGDIR/access.log" 2>/dev/null || true
        log "access_tail_end"
    fi
    if [ -f "$LOGDIR/error.log" ]; then
        log "error_tail_begin"
        tail -n 30 "$LOGDIR/error.log" 2>/dev/null || true
        log "error_tail_end"
    fi
    if [ -f "$LOGDIR/nginx-stdout.log" ]; then
        log "nginx_stdout_tail_begin"
        tail -n 20 "$LOGDIR/nginx-stdout.log" 2>/dev/null || true
        log "nginx_stdout_tail_end"
    fi
}

start_nginx() {
    nginx -t -c "$CONF" -p "$BASE/" || return 1
    nginx -c "$CONF" -p "$BASE/" > "$LOGDIR/nginx-stdout.log" 2>&1 &
    i=0
    while [ "$i" -lt 10 ]; do
        if run_with_timeout 5 curl -fsS -o /dev/null http://127.0.0.1:8080/small.txt >/dev/null 2>&1; then
            return 0
        fi
        i=$((i + 1))
        sleep 1
    done
    return 1
}

probe_short_connections() {
    log "iterations=$ITERATIONS use_timeout=$USE_TIMEOUT timeout_cmd=$TIMEOUT_CMD"
    i=1
    while [ "$i" -le "$ITERATIONS" ]; do
        body="$OUT/short-$i.body"
        meta="$OUT/short-$i.meta"
        err="$OUT/short-$i.err"
        set +e
        run_curl_probe 10 curl -sS -o "$body" -w 'code=%{http_code} size=%{size_download} time=%{time_total}\n' http://127.0.0.1:8080/small.txt > "$meta" 2> "$err"
        rc=$?
        set -e
        meta_line=$(sed -n '1p' "$meta" 2>/dev/null || true)
        [ -n "$meta_line" ] || meta_line='<empty>'
        if [ "$rc" -ne 0 ]; then
            log "failure iteration=$i rc=$rc meta=$meta_line"
            if [ -s "$err" ]; then
                log "curl_stderr_begin"
                tail -n 20 "$err" 2>/dev/null || true
                log "curl_stderr_end"
            fi
            dump_state
            return 1
        fi
        if ! grep -qx 'phase31 short connection debug file' "$body"; then
            size=$(wc -c < "$body" 2>/dev/null || printf '0')
            log "bad_body iteration=$i size=$size meta=$meta_line"
            dump_state
            return 1
        fi
        if [ $((i % 10)) -eq 0 ]; then
            log "progress iteration=$i meta=$meta_line"
        fi
        i=$((i + 1))
    done
    log "completed iterations=$ITERATIONS"
    printf 'NGINX_SHORT_CONN_DEBUG_PASSED\n'
}

init_timeout_cmd
trap cleanup_nginx EXIT INT TERM
runner_ensure_packages || fail "prepare packages"
prepare_tree || fail "prepare tree"
start_nginx || { dump_state; fail "start nginx"; }
probe_short_connections || fail "short connections"
cleanup_nginx
