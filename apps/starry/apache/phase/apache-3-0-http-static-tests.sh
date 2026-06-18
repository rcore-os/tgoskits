#!/bin/sh
set -eu

BASE=/tmp/apache-phase30
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
APP_DIR=$(dirname "$SCRIPT_DIR")
CONF="$BASE/conf/static.conf"
DOCROOT="$BASE/htdocs"
LOGDIR="$BASE/logs"
RUNDIR="$BASE/run"
OUT="$BASE/out"
HTTPD_PID=

if [ -f /usr/bin/apache-alpine-mirror.sh ]; then
    . /usr/bin/apache-alpine-mirror.sh
elif [ -f "$APP_DIR/apache-alpine-mirror.sh" ]; then
    . "$APP_DIR/apache-alpine-mirror.sh"
fi

if [ -f /usr/bin/apache-runner-lib.sh ]; then
    . /usr/bin/apache-runner-lib.sh
elif [ -f "$APP_DIR/runner/apache-runner-lib.sh" ]; then
    . "$APP_DIR/runner/apache-runner-lib.sh"
fi

log() { printf 'APACHE_PHASE30_LOG: %s\n' "$*"; }
fail() { printf 'APACHE_PHASE30_TEST_FAILED\n'; log "$*"; exit 1; }
pass_step() { printf 'APACHE_PHASE30_STEP_PASS: %s\n' "$*"; }

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
    printf '=== APACHE_PHASE30_DIAG_BEGIN ===\n'
    date 2>&1 || true
    uname -a 2>&1 || true
    ps 2>&1 || true
    ls -la "$BASE" "$DOCROOT" "$LOGDIR" "$OUT" 2>&1 || true
    dump_file "apache config" "$CONF"
    dump_file "apache stdout" "$LOGDIR/httpd-stdout.log"
    dump_file "apache error log" "$LOGDIR/error.log"
    dump_file "apache access log" "$LOGDIR/access.log"
    printf '=== APACHE_PHASE30_DIAG_END ===\n'
}

cleanup() {
    if [ -n "$HTTPD_PID" ] && kill -0 "$HTTPD_PID" 2>/dev/null; then
        kill -TERM "$HTTPD_PID" 2>/dev/null || true
        i=0
        while kill -0 "$HTTPD_PID" 2>/dev/null && [ "$i" -lt 8 ]; do
            sleep 1
            i=$((i + 1))
        done
        kill -KILL "$HTTPD_PID" 2>/dev/null || true
    fi
    killall -q httpd 2>/dev/null || true
}

finish() {
    status=$?
    if [ "$status" -ne 0 ]; then
        dump_diag
    fi
    cleanup
    exit "$status"
}

trap finish EXIT

prepare_packages() {
    apache_runner_ensure_packages
}

prepare_tree() {
    rm -rf "$BASE"
    mkdir -p "$BASE/conf" "$DOCROOT/dir" "$LOGDIR" "$RUNDIR" "$OUT"
    printf 'phase30 index\n' > "$DOCROOT/index.html"
    printf 'phase30 small\n' > "$DOCROOT/small.txt"
    : > "$DOCROOT/empty.txt"
    printf 'phase30 dir index\n' > "$DOCROOT/dir/index.html"

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
        DirectoryIndex index.html
    </Directory>
</VirtualHost>
EOF
}

start_httpd() {
    httpd -t -f "$CONF" || return 1
    httpd -X -f "$CONF" > "$LOGDIR/httpd-stdout.log" 2>&1 &
    HTTPD_PID=$!
    i=0
    while [ "$i" -lt 30 ]; do
        if ! kill -0 "$HTTPD_PID" 2>/dev/null; then return 1; fi
        if apache_runner_run_with_timeout 2 curl -fsS -o "$OUT/startup.body" http://127.0.0.1:8080/ >/dev/null 2>&1; then return 0; fi
        sleep 1
        i=$((i + 1))
    done
    return 1
}

test_get_small() {
    apache_runner_run_with_timeout 5 curl -fsS -D "$OUT/small.headers" -o "$OUT/small.body" http://127.0.0.1:8080/small.txt
    grep -qx 'phase30 small' "$OUT/small.body"
    grep -qi '^Content-Length: 14' "$OUT/small.headers"
}

test_get_empty() {
    apache_runner_run_with_timeout 5 curl -fsS -D "$OUT/empty.headers" -o "$OUT/empty.body" http://127.0.0.1:8080/empty.txt
    [ "$(wc -c < "$OUT/empty.body")" -eq 0 ]
    grep -qi '^Content-Length: 0' "$OUT/empty.headers"
}

test_get_dir_slash() {
    apache_runner_run_with_timeout 5 curl -fsS -D "$OUT/dir-slash.headers" -o "$OUT/dir-slash.body" http://127.0.0.1:8080/dir/
    grep -qx 'phase30 dir index' "$OUT/dir-slash.body"
}

test_get_dir_redirect() {
    code=$(apache_runner_run_with_timeout 5 curl -sS -o "$OUT/dir.body" -D "$OUT/dir.headers" -w '%{http_code}' http://127.0.0.1:8080/dir || printf 'curl_failed')
    [ "$code" = "301" ]
    grep -qi '^Location: .*/dir/' "$OUT/dir.headers"
}

test_unknown_method() {
    code=$(apache_runner_run_with_timeout 5 curl -sS -X BAD -o "$OUT/bad.body" -D "$OUT/bad.headers" -w '%{http_code}' http://127.0.0.1:8080/ || printf 'curl_failed')
    [ "$code" = "501" ]
}

test_connection_close() {
    apache_runner_run_with_timeout 5 curl -fsS -H 'Connection: close' -D "$OUT/close.headers" -o "$OUT/close.body" http://127.0.0.1:8080/small.txt
    grep -qx 'phase30 small' "$OUT/close.body"
}

stop_httpd() {
    kill -TERM "$HTTPD_PID"
    i=0
    while kill -0 "$HTTPD_PID" 2>/dev/null && [ "$i" -lt 10 ]; do
        sleep 1
        i=$((i + 1))
    done
    ! kill -0 "$HTTPD_PID" 2>/dev/null
}

run_step() {
    name=$1
    shift
    log "BEGIN $name"
    "$@" || fail "$name"
    pass_step "$name"
}

apache_runner_init_timeout_cmd || fail "timeout command not available"
run_step "prepare packages" prepare_packages
run_step "prepare apache files" prepare_tree
run_step "start apache" start_httpd
run_step "GET /small.txt" test_get_small
run_step "GET /empty.txt" test_get_empty
run_step "GET /dir/" test_get_dir_slash
run_step "GET /dir redirect" test_get_dir_redirect
run_step "unknown method" test_unknown_method
run_step "Connection close" test_connection_close
run_step "stop apache" stop_httpd
printf 'APACHE_PHASE30_TEST_PASSED\n'
