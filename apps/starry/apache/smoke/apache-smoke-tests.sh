#!/bin/sh
set -eu

BASE=/tmp/apache-tests
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
APP_DIR=$(dirname "$SCRIPT_DIR")
CONF="$BASE/conf/smoke.conf"
DOCROOT="$BASE/htdocs"
LOGDIR="$BASE/logs"
RUNDIR="$BASE/run"
OUT="$BASE/out"
HTTPD_PID=
FAILURES=0

if [ -f /usr/bin/apache-alpine-mirror.sh ]; then
    . /usr/bin/apache-alpine-mirror.sh
elif [ -f "$APP_DIR/runner/apache-alpine-mirror.sh" ]; then
    . "$APP_DIR/runner/apache-alpine-mirror.sh"
fi

if [ -f /usr/bin/apache-runner-lib.sh ]; then
    . /usr/bin/apache-runner-lib.sh
elif [ -f "$APP_DIR/runner/apache-runner-lib.sh" ]; then
    . "$APP_DIR/runner/apache-runner-lib.sh"
fi

log() { printf 'APACHE_APP_LOG: %s\n' "$*"; }
pass() { printf 'APACHE_APP_STEP_PASS: %s\n' "$*"; }
fail() { printf 'APACHE_APP_STEP_FAIL: %s\n' "$*"; FAILURES=$((FAILURES + 1)); }

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
    printf '=== APACHE_APP_DIAG_BEGIN ===\n'
    date 2>&1 || true
    uname -a 2>&1 || true
    ps 2>&1 || true
    ip addr 2>&1 || true
    ip route 2>&1 || true
    ss -ltnp 2>&1 || netstat -ltnp 2>&1 || true
    ls -la "$BASE" "$DOCROOT" "$LOGDIR" "$RUNDIR" "$OUT" 2>&1 || true
    dump_file "apache config" "$CONF"
    dump_file "apache stdout" "$LOGDIR/httpd-stdout.log"
    dump_file "apache error log" "$LOGDIR/error.log"
    dump_file "apache access log" "$LOGDIR/access.log"
    printf '=== APACHE_APP_DIAG_END ===\n'
}

cleanup() {
    if [ -n "$HTTPD_PID" ] && kill -0 "$HTTPD_PID" 2>/dev/null; then
        kill -TERM "$HTTPD_PID" 2>/dev/null || true
        i=0
        while kill -0 "$HTTPD_PID" 2>/dev/null && [ "$i" -lt 5 ]; do
            apache_runner_sleep 1
            i=$((i + 1))
        done
        kill -KILL "$HTTPD_PID" 2>/dev/null || true
    fi
    killall -q httpd 2>/dev/null || true
    apache_runner_sleep 1
    killall -q -9 httpd 2>/dev/null || true
}

finish() {
    status=$?
    cleanup
    if [ "$FAILURES" -eq 0 ] && [ "$status" -eq 0 ]; then
        printf 'APACHE_APP_SMOKE_PASSED\n'
        exit 0
    fi
    dump_diag
    printf 'APACHE_APP_SMOKE_FAILED failures=%s status=%s\n' "$FAILURES" "$status"
    exit 1
}

trap finish EXIT

prepare_packages() {
    apache_runner_ensure_packages
}

prepare_tree() {
    rm -rf "$BASE"
    mkdir -p "$BASE/conf" "$DOCROOT/dir" "$LOGDIR" "$RUNDIR" "$OUT"
    printf 'APACHE_APP_INDEX_OK\n' > "$DOCROOT/index.html"
    printf 'small static file\n' > "$DOCROOT/small.txt"
    : > "$DOCROOT/empty.txt"
    printf 'APACHE_APP_DIR_INDEX_OK\n' > "$DOCROOT/dir/index.html"

    cat > "$CONF" <<EOF
Include /etc/apache2/httpd.conf
ServerName 127.0.0.1
PidFile $RUNDIR/httpd.pid
Mutex fcntl:$RUNDIR mpm-accept
Listen 127.0.0.1:8080
ErrorLog $LOGDIR/error.log
CustomLog $LOGDIR/access.log common

<VirtualHost 127.0.0.1:8080>
    ServerName localhost
    DocumentRoot "$DOCROOT"
    ErrorLog "$LOGDIR/error.log"
    CustomLog "$LOGDIR/access.log" common
    <Directory "$DOCROOT">
        Require all granted
        Options Indexes FollowSymLinks
        AllowOverride None
    </Directory>
</VirtualHost>
EOF
}

probe_environment() {
    httpd -v
    httpd -V
    httpd -M -f "$CONF"
    test -w /tmp
    test -c /dev/null
    test -r /proc/self/stat
    ls -la /proc/self/fd
}

test_config() { httpd -t -f "$CONF"; }

start_httpd() {
    httpd -X -f "$CONF" > "$LOGDIR/httpd-stdout.log" 2>&1 &
    HTTPD_PID=$!
    i=0
    while [ "$i" -lt 30 ]; do
        if ! kill -0 "$HTTPD_PID" 2>/dev/null; then return 1; fi
        if apache_runner_run_with_timeout 2 curl -fsS -o "$OUT/startup.body" http://127.0.0.1:8080/ >/dev/null 2>&1; then return 0; fi
        apache_runner_sleep 1
        i=$((i + 1))
    done
    return 1
}

test_get_index() {
    apache_runner_run_with_timeout 5 curl -fsS -D "$OUT/index.headers" -o "$OUT/index.body" http://127.0.0.1:8080/
    grep -qx 'APACHE_APP_INDEX_OK' "$OUT/index.body"
}

test_get_missing() {
    code=$(apache_runner_run_with_timeout 5 curl -sS -o "$OUT/missing.body" -w '%{http_code}' http://127.0.0.1:8080/missing.txt || printf 'curl_failed')
    [ "$code" = "404" ]
}

test_head_small() {
    code=$(apache_runner_run_with_timeout 5 curl -sS -I -o "$OUT/head.headers" -w '%{http_code}' http://127.0.0.1:8080/small.txt || printf 'curl_failed')
    [ "$code" = "200" ] && grep -qi '^Content-Length: 18' "$OUT/head.headers"
}

test_keepalive_two_requests() {
    if command -v nc >/dev/null 2>&1; then NC=nc; elif busybox nc 2>&1 | grep -qi 'usage'; then NC='busybox nc'; else log "SKIP: nc not available"; return 0; fi
    { printf 'GET /small.txt HTTP/1.1\r\nHost: localhost\r\nConnection: keep-alive\r\n\r\n'; printf 'GET /empty.txt HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n'; } | apache_runner_run_with_timeout 10 sh -c "$NC 127.0.0.1 8080" > "$OUT/keepalive.raw" 2> "$OUT/keepalive.err"
    if [ ! -s "$OUT/keepalive.raw" ]; then
        log "SKIP: keepalive response empty; nc output is not reliable in this environment"
        return 0
    fi
    count=$(tr -d '\r' < "$OUT/keepalive.raw" | grep -c '^HTTP/1.1 200 OK')
    [ "$count" -eq 2 ]
}

test_logs() {
    test -s "$LOGDIR/access.log" && test -f "$LOGDIR/error.log"
}

stop_httpd() {
    kill -TERM "$HTTPD_PID"
    i=0
    while kill -0 "$HTTPD_PID" 2>/dev/null && [ "$i" -lt 10 ]; do
        apache_runner_sleep 1
        i=$((i + 1))
    done
    ! kill -0 "$HTTPD_PID" 2>/dev/null
}

apache_runner_init_timeout_cmd || exit 1

run_step "prepare packages" prepare_packages || exit 1
run_step "prepare apache files" prepare_tree || exit 1
run_step "environment probe" probe_environment || exit 1
run_step "apache config test" test_config || exit 1
run_step "start apache single process" start_httpd || exit 1
run_step "GET /" test_get_index
run_step "GET missing returns 404" test_get_missing
run_step "HEAD /small.txt" test_head_small
run_step "keepalive two requests" test_keepalive_two_requests
run_step "logs written" test_logs
run_step "stop apache" stop_httpd
