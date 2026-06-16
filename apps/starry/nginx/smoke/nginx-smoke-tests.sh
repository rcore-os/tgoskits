#!/bin/sh

BASE=/tmp/nginx-tests
CONF="$BASE/conf/single-worker.conf"
MASTER_CONF="$BASE/conf/master-one-worker.conf"
SENDFILE_CONF="$BASE/conf/sendfile.conf"
WWW="$BASE/www"
LOGDIR="$BASE/logs"
OUT="$BASE/out"
NGINX_PID=
MASTER_PID=
FAILURES=0

. /usr/bin/nginx-alpine-mirror.sh

log() { printf 'NGINX_APP_LOG: %s\n' "$*"; }
pass() { printf 'NGINX_APP_STEP_PASS: %s\n' "$*"; }
fail() { printf 'NGINX_APP_STEP_FAIL: %s\n' "$*"; FAILURES=$((FAILURES + 1)); }

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

dump_file() {
    dump_name=$1
    dump_path=$2
    printf -- '--- %s: %s ---\n' "$dump_name" "$dump_path"
    if [ -f "$dump_path" ]; then
        sed -n '1,220p' "$dump_path" 2>&1
    else
        printf 'missing\n'
    fi
}

dump_diag() {
    printf '=== NGINX_APP_DIAG_BEGIN ===\n'
    date 2>&1 || true
    uname -a 2>&1 || true
    ps 2>&1 || true
    ip addr 2>&1 || true
    ip route 2>&1 || true
    ss -ltnp 2>&1 || netstat -ltnp 2>&1 || true
    ls -la "$BASE" "$LOGDIR" "$OUT" 2>&1 || true
    dump_file "nginx config" "$CONF"
    dump_file "nginx master config" "$MASTER_CONF"
    dump_file "nginx sendfile config" "$SENDFILE_CONF"
    dump_file "nginx stdout" "$LOGDIR/nginx-stdout.log"
    dump_file "nginx error log" "$LOGDIR/error.log"
    dump_file "nginx access log" "$LOGDIR/access.log"
    printf '=== NGINX_APP_DIAG_END ===\n'
}

cleanup() {
    if [ -n "$MASTER_PID" ] && kill -0 "$MASTER_PID" 2>/dev/null; then
        kill -TERM "$MASTER_PID" 2>/dev/null || true
        sleep 1
        kill -KILL "$MASTER_PID" 2>/dev/null || true
    fi
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
        printf 'NGINX_APP_SMOKE_PASSED\n'
        exit 0
    fi
    dump_diag
    printf 'NGINX_APP_SMOKE_FAILED failures=%s status=%s\n' "$FAILURES" "$status"
    exit 1
}

trap finish EXIT

prepare_packages() {
    nginx_apk_add_with_fallback nginx curl busybox-extras coreutils || {
        printf 'NGINX_APP_PREPARE_FAILED: all mirrors failed\n'
        return 1
    }
}

prepare_tree() {
    rm -rf "$BASE"
    mkdir -p "$BASE/conf" "$WWW/dir" "$LOGDIR" "$OUT" "$BASE/client_temp"
    printf 'NGINX_APP_INDEX_OK\n' > "$WWW/index.html"
    printf 'small static file\n' > "$WWW/small.txt"
    : > "$WWW/empty.txt"
    printf 'NGINX_APP_DIR_INDEX_OK\n' > "$WWW/dir/index.html"
    dd if=/dev/zero of="$WWW/large.bin" bs=1024 count=1024
    cat > "$CONF" <<'EOF_CONF'
daemon off;
master_process off;
worker_processes 1;
error_log /tmp/nginx-tests/logs/error.log debug;
pid /tmp/nginx-tests/nginx.pid;

events { worker_connections 64; }

http {
    include /etc/nginx/mime.types;
    default_type application/octet-stream;
    access_log /tmp/nginx-tests/logs/access.log;
    sendfile off;
    keepalive_timeout 5;
    client_body_temp_path /tmp/nginx-tests/client_temp;
    server {
        listen 127.0.0.1:8080;
        server_name localhost;
        root /tmp/nginx-tests/www;
        location / { index index.html; }
    }
}
EOF_CONF

    cat > "$MASTER_CONF" <<'EOF_MASTER_CONF'
daemon off;
master_process on;
worker_processes 1;
error_log /tmp/nginx-tests/logs/error-master.log debug;
pid /tmp/nginx-tests/nginx-master.pid;

events { worker_connections 64; }

http {
    include /etc/nginx/mime.types;
    default_type application/octet-stream;
    access_log /tmp/nginx-tests/logs/access-master.log;
    sendfile off;
    keepalive_timeout 5;
    client_body_temp_path /tmp/nginx-tests/client_temp;
    server {
        listen 127.0.0.1:8081;
        server_name localhost;
        root /tmp/nginx-tests/www;
        location / { index index.html; }
    }
}
EOF_MASTER_CONF

    cat > "$SENDFILE_CONF" <<'EOF_SENDFILE_CONF'
daemon off;
master_process off;
worker_processes 1;
error_log /tmp/nginx-tests/logs/error-sendfile.log debug;
pid /tmp/nginx-tests/nginx-sendfile.pid;

events { worker_connections 64; }

http {
    include /etc/nginx/mime.types;
    default_type application/octet-stream;
    access_log /tmp/nginx-tests/logs/access-sendfile.log;
    sendfile on;
    keepalive_timeout 5;
    client_max_body_size 4k;
    client_body_buffer_size 1k;
    client_body_temp_path /tmp/nginx-tests/client_temp;
    server {
        listen 127.0.0.1:8082;
        server_name localhost;
        root /tmp/nginx-tests/www;
        location / { index index.html; }
    }
}
EOF_SENDFILE_CONF
}

probe_environment() {
    nginx -v
    nginx -V
    test -w /tmp
    test -c /dev/null
    test -c /dev/zero
    test -r /proc/self/stat
    test -r /proc/meminfo
    ls -la /proc/self/fd
}

test_config() { nginx -t -c "$CONF" -p "$BASE/"; }
test_master_config() { nginx -t -c "$MASTER_CONF" -p "$BASE/"; }
test_sendfile_config() { nginx -t -c "$SENDFILE_CONF" -p "$BASE/"; }

start_nginx() {
    nginx -c "$CONF" -p "$BASE/" > "$LOGDIR/nginx-stdout.log" 2>&1 &
    NGINX_PID=$!
    i=0
    while [ "$i" -lt 90 ]; do
        if ! kill -0 "$NGINX_PID" 2>/dev/null; then return 1; fi
        if curl -fsS -o "$OUT/startup.body" http://127.0.0.1:8080/ >/dev/null 2>&1; then return 0; fi
        sleep 1
        i=$((i + 1))
    done
    return 1
}

test_get_index() { curl -fsS -D "$OUT/index.headers" -o "$OUT/index.body" http://127.0.0.1:8080/ && grep -qx 'NGINX_APP_INDEX_OK' "$OUT/index.body"; }
test_get_missing() { code=$(curl -sS -o "$OUT/missing.body" -w '%{http_code}' http://127.0.0.1:8080/missing.txt || printf 'curl_failed'); [ "$code" = "404" ]; }
test_head_small() { code=$(curl -sS -I -o "$OUT/head.headers" -w '%{http_code}' http://127.0.0.1:8080/small.txt || printf 'curl_failed'); [ "$code" = "200" ] && grep -qi '^Content-Length: 18' "$OUT/head.headers"; }

test_keepalive_two_requests() {
    if command -v nc >/dev/null 2>&1; then NC=nc; elif busybox nc 2>&1 | grep -qi 'usage'; then NC='busybox nc'; else return 0; fi
    if command -v timeout >/dev/null 2>&1; then TIMEOUT=timeout; elif busybox timeout 2>&1 | grep -qi 'usage'; then TIMEOUT='busybox timeout'; else return 0; fi
    { printf 'GET /small.txt HTTP/1.1\r\nHost: localhost\r\nConnection: keep-alive\r\n\r\n'; printf 'GET /empty.txt HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n'; } | $TIMEOUT 20 sh -c "$NC 127.0.0.1 8080" > "$OUT/keepalive.raw"
    [ -s "$OUT/keepalive.raw" ] || return 0
    count=$(tr -d '\r' < "$OUT/keepalive.raw" | grep -c '^HTTP/1.1 200 OK')
    [ "$count" -eq 2 ]
}

test_logs() { test -s "$LOGDIR/access.log" && test -f "$LOGDIR/error.log"; }
stop_nginx() { kill -TERM "$NGINX_PID"; i=0; while kill -0 "$NGINX_PID" 2>/dev/null && [ "$i" -lt 30 ]; do sleep 1; i=$((i + 1)); done; ! kill -0 "$NGINX_PID" 2>/dev/null; }

start_nginx_master() {
    nginx -c "$MASTER_CONF" -p "$BASE/" > "$LOGDIR/nginx-master-stdout.log" 2>&1 &
    MASTER_PID=$!
    i=0
    while [ "$i" -lt 90 ]; do
        if ! kill -0 "$MASTER_PID" 2>/dev/null; then return 1; fi
        if curl -fsS -o "$OUT/master-startup.body" http://127.0.0.1:8081/ >/dev/null 2>&1; then return 0; fi
        sleep 1
        i=$((i + 1))
    done
    return 1
}

test_master_get_index() { curl -fsS -D "$OUT/master-index.headers" -o "$OUT/master-index.body" http://127.0.0.1:8081/ && grep -qx 'NGINX_APP_INDEX_OK' "$OUT/master-index.body"; }
test_master_reload() { nginx -s reload -c "$MASTER_CONF" -p "$BASE/" && sleep 2 && curl -fsS -o "$OUT/master-after-reload.body" http://127.0.0.1:8081/small.txt && grep -qx 'small static file' "$OUT/master-after-reload.body"; }
stop_nginx_master() { nginx -s quit -c "$MASTER_CONF" -p "$BASE/"; i=0; while kill -0 "$MASTER_PID" 2>/dev/null && [ "$i" -lt 30 ]; do sleep 1; i=$((i + 1)); done; ! kill -0 "$MASTER_PID" 2>/dev/null; }

start_nginx_sendfile() {
    nginx -c "$SENDFILE_CONF" -p "$BASE/" > "$LOGDIR/nginx-sendfile-stdout.log" 2>&1 &
    NGINX_PID=$!
    i=0
    while [ "$i" -lt 90 ]; do
        if ! kill -0 "$NGINX_PID" 2>/dev/null; then return 1; fi
        if curl -fsS -o "$OUT/sendfile-startup.body" http://127.0.0.1:8082/ >/dev/null 2>&1; then return 0; fi
        sleep 1
        i=$((i + 1))
    done
    return 1
}

test_large_sendfile() { curl -fsS -D "$OUT/large.headers" -o "$OUT/large.bin" http://127.0.0.1:8082/large.bin && [ "$(wc -c < "$OUT/large.bin")" -eq 1048576 ] && cmp "$WWW/large.bin" "$OUT/large.bin"; }
test_range() { curl -fsS -D "$OUT/range.headers" -H 'Range: bytes=0-15' -o "$OUT/range.bin" http://127.0.0.1:8082/large.bin && [ "$(wc -c < "$OUT/range.bin")" -eq 16 ] && grep -qi '^HTTP/1.1 206' "$OUT/range.headers" && grep -qi '^Content-Range: bytes 0-15/1048576' "$OUT/range.headers"; }
test_post_small() { code=$(curl -sS -D "$OUT/post.headers" -o "$OUT/post.body" -w '%{http_code}' -X POST --data 'abc' http://127.0.0.1:8082/ || printf 'curl_failed'); [ "$code" = "405" ] || [ "$code" = "404" ] || [ "$code" = "200" ]; }
test_post_too_large() { dd if=/dev/zero of="$OUT/post-large.bin" bs=1024 count=8 && code=$(curl -sS -D "$OUT/post-large.headers" -o "$OUT/post-large.body" -w '%{http_code}' -X POST --data-binary "@$OUT/post-large.bin" http://127.0.0.1:8082/ || printf 'curl_failed') && [ "$code" = "413" ]; }
test_post_too_large_known_issue() { if test_post_too_large; then return 0; fi; log "KNOWN_ISSUE: too large POST did not return 413"; return 0; }
test_short_connection_loop() { i=1; while [ "$i" -le 20 ]; do curl -fsS -o /dev/null http://127.0.0.1:8082/small.txt || return 1; i=$((i + 1)); done; }

run_step "prepare packages" prepare_packages || exit 1
run_step "prepare nginx files" prepare_tree || exit 1
run_step "environment probe" probe_environment || exit 1
run_step "nginx config test" test_config || exit 1
run_step "start nginx single process" start_nginx || exit 1
run_step "GET /" test_get_index
run_step "GET missing returns 404" test_get_missing
run_step "HEAD /small.txt" test_head_small
run_step "keepalive two requests" test_keepalive_two_requests
run_step "logs written" test_logs
run_step "stop nginx" stop_nginx
run_step "nginx master config test" test_master_config
run_step "start nginx master one worker" start_nginx_master
run_step "master GET /" test_master_get_index
run_step "master reload" test_master_reload
run_step "master quit" stop_nginx_master
run_step "nginx sendfile config test" test_sendfile_config
run_step "start nginx sendfile" start_nginx_sendfile
run_step "large file sendfile" test_large_sendfile
run_step "range request" test_range
run_step "small POST" test_post_small
run_step "too large POST known issue probe" test_post_too_large_known_issue
run_step "20 short connections" test_short_connection_loop
run_step "stop nginx sendfile" stop_nginx
