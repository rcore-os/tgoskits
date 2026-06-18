#!/bin/sh
set -eu

BASE=/tmp/apache-phase55
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
APP_DIR=$(dirname "$SCRIPT_DIR")
CONF_OFF="$BASE/conf/sendfile-off.conf"
CONF_ON="$BASE/conf/sendfile-on.conf"
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

log() { printf 'APACHE_PHASE55_LOG: %s\n' "$*"; }
fail() { printf 'APACHE_PHASE55_TEST_FAILED\n'; log "$*"; exit 1; }
pass_step() { printf 'APACHE_PHASE55_STEP_PASS: %s\n' "$*"; }

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
    printf '=== APACHE_PHASE55_DIAG_BEGIN ===\n'
    date 2>&1 || true
    uname -a 2>&1 || true
    ps 2>&1 || true
    ls -la "$BASE" "$DOCROOT" "$LOGDIR" "$RUNDIR" "$OUT" 2>&1 || true
    dump_file "apache off config" "$CONF_OFF"
    dump_file "apache on config" "$CONF_ON"
    dump_file "apache stdout" "$LOGDIR/httpd-stdout.log"
    dump_file "apache error log" "$LOGDIR/error.log"
    dump_file "apache access log" "$LOGDIR/access.log"
    printf '=== APACHE_PHASE55_DIAG_END ===\n'
}

cleanup() {
    if [ -n "$HTTPD_PID" ] && kill -0 "$HTTPD_PID" 2>/dev/null; then
        kill -TERM "$HTTPD_PID" 2>/dev/null || true
        i=0
        while kill -0 "$HTTPD_PID" 2>/dev/null && [ "$i" -lt 10 ]; do
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
    mkdir -p "$BASE/conf" "$DOCROOT" "$LOGDIR" "$RUNDIR" "$OUT"
    printf 'phase55 index\n' > "$DOCROOT/index.html"
    dd if=/dev/zero of="$DOCROOT/large.bin" bs=1024 count=1024 2>/dev/null | tr '\0' 'A' >/dev/null

    cat > "$CONF_OFF" <<EOF
Include /etc/apache2/httpd.conf
ServerName 127.0.0.1
PidFile $RUNDIR/httpd-off.pid
Mutex fcntl:$RUNDIR mpm-accept
Listen 127.0.0.1:8080
ErrorLog $LOGDIR/error.log
CustomLog $LOGDIR/access.log common
FileETag MTime Size

<VirtualHost 127.0.0.1:8080>
    ServerName localhost
    DocumentRoot "$DOCROOT"
    ErrorLog "$LOGDIR/error.log"
    CustomLog "$LOGDIR/access.log" common
    EnableSendfile Off
    EnableMMAP Off
    <Directory "$DOCROOT">
        Require all granted
        Options +Indexes +FollowSymLinks
        AllowOverride None
        DirectoryIndex index.html
    </Directory>
</VirtualHost>
EOF

    cat > "$CONF_ON" <<EOF
Include /etc/apache2/httpd.conf
ServerName 127.0.0.1
PidFile $RUNDIR/httpd-on.pid
Mutex fcntl:$RUNDIR mpm-accept
Listen 127.0.0.1:8080
ErrorLog $LOGDIR/error.log
CustomLog $LOGDIR/access.log common
FileETag MTime Size

<VirtualHost 127.0.0.1:8080>
    ServerName localhost
    DocumentRoot "$DOCROOT"
    ErrorLog "$LOGDIR/error.log"
    CustomLog "$LOGDIR/access.log" common
    EnableSendfile On
    EnableMMAP On
    <Directory "$DOCROOT">
        Require all granted
        Options +Indexes +FollowSymLinks
        AllowOverride None
        DirectoryIndex index.html
    </Directory>
</VirtualHost>
EOF
}

start_httpd() {
    conf=$1
    pidfile=$2
    httpd -t -f "$conf" || return 1
    httpd -k start -f "$conf" > "$LOGDIR/httpd-stdout.log" 2>&1 || return 1
    i=0
    while [ "$i" -lt 30 ]; do
        if [ -f "$pidfile" ]; then
            HTTPD_PID=$(cat "$pidfile")
            if kill -0 "$HTTPD_PID" 2>/dev/null; then
                if apache_runner_run_with_timeout 2 curl -fsS -o "$OUT/startup.body" http://127.0.0.1:8080/ >/dev/null 2>&1; then return 0; fi
            fi
        fi
        sleep 1
        i=$((i + 1))
    done
    return 1
}

stop_httpd() {
    httpd -k stop -f "$1" >/dev/null 2>&1 || kill -TERM "$HTTPD_PID"
    i=0
    while kill -0 "$HTTPD_PID" 2>/dev/null && [ "$i" -lt 10 ]; do
        sleep 1
        i=$((i + 1))
    done
    ! kill -0 "$HTTPD_PID" 2>/dev/null
}

test_sendfile_off_large() {
    apache_runner_run_with_timeout 10 curl -fsS -D "$OUT/off.headers" -o "$OUT/off.bin" http://127.0.0.1:8080/large.bin
    [ "$(wc -c < "$OUT/off.bin")" -eq 1048576 ]
    cmp "$DOCROOT/large.bin" "$OUT/off.bin"
}

test_sendfile_on_large() {
    apache_runner_run_with_timeout 10 curl -fsS -D "$OUT/on.headers" -o "$OUT/on.bin" http://127.0.0.1:8080/large.bin
    [ "$(wc -c < "$OUT/on.bin")" -eq 1048576 ]
    cmp "$DOCROOT/large.bin" "$OUT/on.bin"
}

test_range_requests() {
    code=$(apache_runner_run_with_timeout 10 curl -sS -D "$OUT/range-0-15.headers" -o "$OUT/range-0-15.bin" -w '%{http_code}' -H 'Range: bytes=0-15' http://127.0.0.1:8080/large.bin || printf 'curl_failed')
    [ "$code" = "206" ]
    [ "$(wc -c < "$OUT/range-0-15.bin")" -eq 16 ]
    grep -qi '^Content-Range: bytes 0-15/1048576' "$OUT/range-0-15.headers"

    code=$(apache_runner_run_with_timeout 10 curl -sS -D "$OUT/range-100-199.headers" -o "$OUT/range-100-199.bin" -w '%{http_code}' -H 'Range: bytes=100-199' http://127.0.0.1:8080/large.bin || printf 'curl_failed')
    [ "$code" = "206" ]
    [ "$(wc -c < "$OUT/range-100-199.bin")" -eq 100 ]
    grep -qi '^Content-Range: bytes 100-199/1048576' "$OUT/range-100-199.headers"

    code=$(apache_runner_run_with_timeout 10 curl -sS -D "$OUT/range-suffix.headers" -o "$OUT/range-suffix.bin" -w '%{http_code}' -H 'Range: bytes=-64' http://127.0.0.1:8080/large.bin || printf 'curl_failed')
    [ "$code" = "206" ]
    [ "$(wc -c < "$OUT/range-suffix.bin")" -eq 64 ]
    grep -qi '^Content-Range: bytes 1048512-1048575/1048576' "$OUT/range-suffix.headers"
}

test_conditional_get() {
    apache_runner_run_with_timeout 10 curl -fsS -I -o "$OUT/head.headers" http://127.0.0.1:8080/large.bin
    grep -qi '^Last-Modified:' "$OUT/head.headers"
    grep -qi '^ETag:' "$OUT/head.headers"
    code=$(apache_runner_run_with_timeout 10 curl -sS -D "$OUT/conditional.headers" -o "$OUT/conditional.body" -w '%{http_code}' -z "$DOCROOT/large.bin" http://127.0.0.1:8080/large.bin || printf 'curl_failed')
    [ "$code" = "304" ]
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

log "BEGIN sendfile off"
start_httpd "$CONF_OFF" "$RUNDIR/httpd-off.pid" || fail "sendfile off start"
HTTPD_PID=$(cat "$RUNDIR/httpd-off.pid")
run_step "sendfile off large" test_sendfile_off_large
stop_httpd "$CONF_OFF" || fail "sendfile off stop"

log "BEGIN sendfile on"
start_httpd "$CONF_ON" "$RUNDIR/httpd-on.pid" || fail "sendfile on start"
HTTPD_PID=$(cat "$RUNDIR/httpd-on.pid")
run_step "sendfile on large" test_sendfile_on_large
run_step "range requests" test_range_requests
run_step "conditional get" test_conditional_get
stop_httpd "$CONF_ON" || fail "sendfile on stop"

printf 'APACHE_PHASE55_TEST_PASSED\n'
