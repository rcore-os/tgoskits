#!/bin/sh
set -eu

. /usr/bin/nginx-runner-lib.sh

BASE=/tmp/nginx-x86-timing-debug
CONF="$BASE/conf/timing.conf"
WWW="$BASE/www"
LOGDIR="$BASE/logs"
TIMEOUT_CMD=
LOOP_ITERS="${NGINX_X86_TIMING_LOOP_ITERS:-100}"
CURL_ITERS="${NGINX_X86_TIMING_CURL_ITERS:-40}"
SLEEP_ITERS="${NGINX_X86_TIMING_SLEEP_ITERS:-5}"

log() { printf 'NGINX_X86_TIMING_DEBUG_LOG: %s\n' "$*"; }
fail() { printf 'NGINX_X86_TIMING_DEBUG_FAILED\n'; log "$*"; exit 1; }

now_s() {
    cut -d. -f1 /proc/uptime
}

elapsed_s() {
    start=$1
    end=$(now_s)
    printf '%s' $((end - start))
}

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

prepare_nginx() {
    rm -rf "$BASE"
    mkdir -p "$BASE/conf" "$WWW" "$LOGDIR"
    printf 'x86 timing debug file\n' > "$WWW/small.txt"
    cat > "$CONF" <<'EOF'
daemon off;
master_process off;
worker_processes 1;
error_log /tmp/nginx-x86-timing-debug/logs/error.log notice;
pid /tmp/nginx-x86-timing-debug/nginx.pid;
events { worker_connections 64; }
http { include /etc/nginx/mime.types; access_log off; sendfile off; keepalive_timeout 5; server { listen 127.0.0.1:8080; root /tmp/nginx-x86-timing-debug/www; location / { index index.html; } } }
EOF
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

bench_shell_arithmetic() {
    start=$(now_s)
    i=0
    while [ "$i" -lt "$LOOP_ITERS" ]; do
        i=$((i + 1))
    done
    log "bench=shell_arithmetic iterations=$LOOP_ITERS elapsed_s=$(elapsed_s "$start")"
}

bench_shell_builtin() {
    start=$(now_s)
    i=0
    while [ "$i" -lt "$LOOP_ITERS" ]; do
        :
        i=$((i + 1))
    done
    log "bench=shell_builtin iterations=$LOOP_ITERS elapsed_s=$(elapsed_s "$start")"
}

bench_command_loop() {
    name=$1
    iterations=$2
    shift 2
    start=$(now_s)
    i=0
    while [ "$i" -lt "$iterations" ]; do
        "$@" >/dev/null 2>&1 || return 1
        i=$((i + 1))
    done
    log "bench=$name iterations=$iterations elapsed_s=$(elapsed_s "$start") cmd=$*"
}

bench_true() {
    bench_command_loop true "$LOOP_ITERS" /bin/true
}

bench_timeout_true() {
    start=$(now_s)
    i=0
    while [ "$i" -lt "$LOOP_ITERS" ]; do
        run_with_timeout 5 /bin/true || return 1
        i=$((i + 1))
    done
    log "bench=timeout_true iterations=$LOOP_ITERS elapsed_s=$(elapsed_s "$start") timeout_cmd=$TIMEOUT_CMD"
}

bench_curl() {
    name=$1
    use_timeout=$2
    start=$(now_s)
    i=0
    while [ "$i" -lt "$CURL_ITERS" ]; do
        if [ "$use_timeout" = "1" ]; then
            run_with_timeout 10 curl -fsS -o /dev/null http://127.0.0.1:8080/small.txt >/dev/null 2>&1 || return 1
        else
            curl -fsS -o /dev/null http://127.0.0.1:8080/small.txt >/dev/null 2>&1 || return 1
        fi
        i=$((i + 1))
    done
    log "bench=$name iterations=$CURL_ITERS elapsed_s=$(elapsed_s "$start")"
}

init_timeout_cmd
trap cleanup_nginx EXIT INT TERM
runner_ensure_packages || fail "prepare packages"
bench_shell_arithmetic || fail "shell arithmetic"
bench_shell_builtin || fail "shell builtin"
bench_true || fail "true loop"
bench_command_loop busybox_true "$LOOP_ITERS" /bin/busybox true || fail "busybox true loop"
bench_command_loop echo "$LOOP_ITERS" /bin/echo ok || fail "echo loop"
bench_command_loop sleep0 "$LOOP_ITERS" /bin/sleep 0 || fail "sleep0 loop"
bench_command_loop date 20 /bin/date +%s || fail "date loop"
bench_command_loop sleep1 "$SLEEP_ITERS" /bin/sleep 1 || fail "sleep1 loop"
bench_timeout_true || fail "timeout true loop"
prepare_nginx || fail "start nginx"
bench_curl direct_curl 0 || fail "direct curl loop"
bench_curl timeout_curl 1 || fail "timeout curl loop"
printf 'NGINX_X86_TIMING_DEBUG_PASSED\n'
