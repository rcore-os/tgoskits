#!/bin/sh
set -eu

BASE=/tmp/nginx-bad-method-matrix
CONF="$BASE/conf/nginx.conf"
LOGDIR="$BASE/logs"
OUT="$BASE/out"
WWW="$BASE/www"
TIMEOUT_CMD=

log() { printf 'NGINX_BAD_METHOD_MATRIX_LOG: %s\n' "$*"; }
fail() { printf 'NGINX_BAD_METHOD_MATRIX_FAILED\n'; log "$*"; exit 1; }

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
        if run_with_timeout 40 apk --timeout 40 update && run_with_timeout 40 apk --timeout 40 add nginx curl busybox-extras netcat-openbsd; then return 0; fi
    done
    return 1
}

prepare_tree() {
    rm -rf "$BASE"
    mkdir -p "$BASE/conf" "$LOGDIR" "$OUT" "$WWW"
    printf 'ok\n' > "$WWW/index.html"
    cat > "$CONF" <<'EOF'
daemon off;
master_process off;
worker_processes 1;
error_log /tmp/nginx-bad-method-matrix/logs/error.log debug;
pid /tmp/nginx-bad-method-matrix/nginx.pid;
events { worker_connections 64; }
http {
    include /etc/nginx/mime.types;
    keepalive_timeout 5;
    access_log /tmp/nginx-bad-method-matrix/logs/access.log;
    server {
        listen 127.0.0.1:8080;
        root /tmp/nginx-bad-method-matrix/www;
        location / { index index.html; }
    }
}
EOF
}

start_nginx() {
    nginx -t -c "$CONF" -p "$BASE/" || return 1
    nginx -c "$CONF" -p "$BASE/" > "$LOGDIR/nginx.stdout" 2>&1 &
    i=0
    while [ "$i" -lt 8 ]; do
        run_with_timeout 1 curl -fsS -o /dev/null http://127.0.0.1:8080/index.html >/dev/null 2>&1 && return 0
        i=$((i + 1))
        sleep 1
    done
    return 1
}

one_probe() {
    name=$1
    cmd=$2
    out="$OUT/$name.raw"
    run_with_timeout 5 sh -c "$cmd" > "$out" 2>&1 || true
    tr -d '\r' < "$out" > "$OUT/$name.norm"
    first=$(sed -n '1p' "$OUT/$name.norm" || true)
    [ -n "$first" ] || first='<empty>'
    log "$name first_line=$first"
    if grep -Eq '^HTTP/1.1 (400|405)' "$OUT/$name.norm"; then
        log "$name hit=400_or_405"
    fi
}

run_matrix() {
    code=$(run_with_timeout 3 curl -sS -o /dev/null -w '%{http_code}' http://127.0.0.1:8080/ || true)
    [ -n "$code" ] || code='<empty>'
    log "curl_get_http_code=$code"
    code=$(run_with_timeout 3 curl -sS -o /dev/null -w '%{http_code}' -X BAD http://127.0.0.1:8080/ || true)
    [ -n "$code" ] || code='<empty>'
    log "curl_x_bad_http_code=$code"

    one_probe nc_default "printf 'BAD / HTTP/1.1\\r\\nHost: localhost\\r\\nConnection: close\\r\\n\\r\\n' | nc 127.0.0.1 8080"
    one_probe nc_w2 "printf 'BAD / HTTP/1.1\\r\\nHost: localhost\\r\\nConnection: close\\r\\n\\r\\n' | nc -w 2 127.0.0.1 8080"
    one_probe nc_http10 "printf 'BAD / HTTP/1.0\\r\\nHost: localhost\\r\\nConnection: close\\r\\n\\r\\n' | nc -w 2 127.0.0.1 8080"
    one_probe nc_get "printf 'GET / HTTP/1.1\\r\\nHost: localhost\\r\\nConnection: close\\r\\n\\r\\n' | nc -w 2 127.0.0.1 8080"

    if busybox nc 2>&1 | grep -qi 'usage'; then
        one_probe bb_nc_bad "printf 'BAD / HTTP/1.1\\r\\nHost: localhost\\r\\nConnection: close\\r\\n\\r\\n' | busybox nc -w 2 127.0.0.1 8080"
        one_probe bb_nc_get "printf 'GET / HTTP/1.1\\r\\nHost: localhost\\r\\nConnection: close\\r\\n\\r\\n' | busybox nc -w 2 127.0.0.1 8080"
    fi

    if command -v nc.openbsd >/dev/null 2>&1; then
        one_probe nco_bad "printf 'BAD / HTTP/1.1\\r\\nHost: localhost\\r\\nConnection: close\\r\\n\\r\\n' | nc.openbsd -w 2 127.0.0.1 8080"
        one_probe nco_get "printf 'GET / HTTP/1.1\\r\\nHost: localhost\\r\\nConnection: close\\r\\n\\r\\n' | nc.openbsd -w 2 127.0.0.1 8080"
    fi

    if [ -f "$LOGDIR/error.log" ]; then
        tail_line=$(sed -n '$p' "$LOGDIR/error.log" || true)
        [ -n "$tail_line" ] || tail_line='<empty>'
        log "error_log_tail=$tail_line"
    fi

    printf 'NGINX_BAD_METHOD_MATRIX_DONE\n'
}

init_timeout_cmd
( sleep 120; log "watchdog timeout"; kill -TERM $$ ) &
prepare_packages || fail "prepare packages"
prepare_tree || fail "prepare tree"
start_nginx || fail "start nginx"
run_matrix
cleanup_nginx
