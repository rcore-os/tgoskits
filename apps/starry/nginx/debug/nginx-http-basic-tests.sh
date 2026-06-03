#!/bin/sh

BASE=/tmp/nginx-http-basic
CONF="$BASE/conf/http-basic.conf"
WWW="$BASE/www"
LOGDIR="$BASE/logs"
OUT="$BASE/out"
NGINX_PID=
FAILURES=0
TIMEOUT_CMD=

. /usr/bin/nginx-alpine-mirror.sh

log() { printf 'NGINX_HTTP_BASIC_LOG: %s\n' "$*"; }
pass() { printf 'NGINX_HTTP_BASIC_STEP_PASS: %s\n' "$*"; }
fail() { printf 'NGINX_HTTP_BASIC_STEP_FAIL: %s\n' "$*"; FAILURES=$((FAILURES + 1)); }

run_step() {
    step_name=$1
    shift
    log "BEGIN $step_name"
    if "$@"; then
        pass "$step_name"
        return 0
    fi
    fail "$step_name"
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
    TIMEOUT_CMD=
    return 0
}

run_with_timeout() {
    timeout_sec=$1
    shift
    if [ -n "$TIMEOUT_CMD" ]; then
        $TIMEOUT_CMD "$timeout_sec" "$@"
        return $?
    fi
    "$@"
}

cleanup() {
    if [ -n "$NGINX_PID" ] && kill -0 "$NGINX_PID" 2>/dev/null; then
        kill -TERM "$NGINX_PID" 2>/dev/null || true
        sleep 1
        kill -KILL "$NGINX_PID" 2>/dev/null || true
    fi
}

finish() {
    status=$?
    cleanup
    if [ "$FAILURES" -eq 0 ] && [ "$status" -eq 0 ]; then
        printf 'NGINX_HTTP_BASIC_PASSED\n'
        exit 0
    fi
    printf 'NGINX_HTTP_BASIC_FAILED failures=%s status=%s\n' "$FAILURES" "$status"
    exit 1
}

trap finish EXIT

prepare_packages() {
    nginx_apk_add_with_fallback nginx curl busybox-extras || {
        printf 'NGINX_HTTP_BASIC_PREPARE_FAILED: all mirrors failed\n'
        return 1
    }
}

prepare_tree() {
    rm -rf "$BASE"
    mkdir -p "$BASE/conf" "$WWW/dir" "$LOGDIR" "$OUT" "$BASE/client_temp"
    printf 'HTTP_BASIC_INDEX_OK\n' > "$WWW/index.html"
    printf 'small static file\n' > "$WWW/small.txt"
    : > "$WWW/empty.txt"
    printf 'HTTP_BASIC_DIR_INDEX_OK\n' > "$WWW/dir/index.html"

    cat > "$CONF" <<'EOF_CONF'
daemon off;
master_process off;
worker_processes 1;
error_log /tmp/nginx-http-basic/logs/error.log debug;
pid /tmp/nginx-http-basic/nginx.pid;

events { worker_connections 64; }

http {
    include /etc/nginx/mime.types;
    default_type application/octet-stream;
    access_log /tmp/nginx-http-basic/logs/access.log;
    sendfile off;
    keepalive_timeout 5;
    client_body_temp_path /tmp/nginx-http-basic/client_temp;
    server {
        listen 127.0.0.1:8080;
        server_name localhost;
        root /tmp/nginx-http-basic/www;
        location / { index index.html; }
    }
}
EOF_CONF
}

test_config() { nginx -t -c "$CONF" -p "$BASE/"; }

start_nginx() {
    nginx -c "$CONF" -p "$BASE/" > "$LOGDIR/nginx-stdout.log" 2>&1 &
    NGINX_PID=$!
    i=0
    while [ "$i" -lt 90 ]; do
        if ! kill -0 "$NGINX_PID" 2>/dev/null; then return 1; fi
        if run_with_timeout 8 curl -fsS -o "$OUT/startup.body" http://127.0.0.1:8080/ >/dev/null 2>&1; then return 0; fi
        sleep 1
        i=$((i + 1))
    done
    return 1
}

test_get_small() {
    code=$(run_with_timeout 12 curl -sS -D "$OUT/small.headers" -o "$OUT/small.body" -w '%{http_code}' http://127.0.0.1:8080/small.txt || printf 'curl_failed')
    [ "$code" = "200" ] && grep -qx 'small static file' "$OUT/small.body" && grep -qi '^Content-Length: 18' "$OUT/small.headers"
}

test_get_empty() {
    code=$(run_with_timeout 12 curl -sS -D "$OUT/empty.headers" -o "$OUT/empty.body" -w '%{http_code}' http://127.0.0.1:8080/empty.txt || printf 'curl_failed')
    [ "$code" = "200" ] && [ "$(wc -c < "$OUT/empty.body")" -eq 0 ] && grep -qi '^Content-Length: 0' "$OUT/empty.headers"
}

test_get_dir_slash() {
    code=$(run_with_timeout 12 curl -sS -D "$OUT/dir-slash.headers" -o "$OUT/dir-slash.body" -w '%{http_code}' http://127.0.0.1:8080/dir/ || printf 'curl_failed')
    [ "$code" = "200" ] && grep -qx 'HTTP_BASIC_DIR_INDEX_OK' "$OUT/dir-slash.body"
}

test_get_dir_redirect() {
    code=$(run_with_timeout 12 curl -sS -D "$OUT/dir.headers" -o "$OUT/dir.body" -w '%{http_code}' http://127.0.0.1:8080/dir || printf 'curl_failed')
    [ "$code" = "301" ] || [ "$code" = "302" ]
}

test_bad_method() {
    if command -v nc >/dev/null 2>&1; then
        NC='nc'
    elif busybox nc 2>&1 | grep -qi 'usage'; then
        NC='busybox nc'
    else
        return 0
    fi
    { printf 'BAD / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n'; } | run_with_timeout 12 sh -c "$NC 127.0.0.1 8080" > "$OUT/bad-method.raw"
    tr -d '\r' < "$OUT/bad-method.raw" > "$OUT/bad-method.normalized"
    grep -Eq '^HTTP/1.1 (400|405)' "$OUT/bad-method.normalized"
}

stop_nginx() {
    kill -TERM "$NGINX_PID"
    i=0
    while kill -0 "$NGINX_PID" 2>/dev/null && [ "$i" -lt 30 ]; do
        sleep 1
        i=$((i + 1))
    done
    ! kill -0 "$NGINX_PID" 2>/dev/null
}

run_step "prepare packages" prepare_packages || exit 1
run_step "prepare nginx files" prepare_tree || exit 1
run_step "init timeout helper" init_timeout_cmd || exit 1
run_step "nginx config test" test_config || exit 1
run_step "start nginx" start_nginx || exit 1
run_step "GET /small.txt" test_get_small
run_step "GET /empty.txt" test_get_empty
run_step "GET /dir/" test_get_dir_slash
run_step "GET /dir redirect" test_get_dir_redirect
run_step "BAD method returns 400/405" test_bad_method
run_step "stop nginx" stop_nginx
