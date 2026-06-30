#!/bin/sh
set -eu

BASE=/tmp/apache-phase20
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
APP_DIR=$(dirname "$SCRIPT_DIR")
CONF="$BASE/conf/mpm-prefork.conf"
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

log() { printf 'APACHE_PHASE20_LOG: %s\n' "$*"; }
fail() { printf 'APACHE_PHASE20_TEST_FAILED\n'; log "$*"; exit 1; }
pass_step() { printf 'APACHE_PHASE20_STEP_PASS: %s\n' "$*"; }

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
    printf '=== APACHE_PHASE20_DIAG_BEGIN ===\n'
    date 2>&1 || true
    uname -a 2>&1 || true
    ps 2>&1 || true
    ls -la "$BASE" "$DOCROOT" "$LOGDIR" "$RUNDIR" "$OUT" 2>&1 || true
    dump_file "apache config" "$CONF"
    dump_file "apache stdout" "$LOGDIR/httpd-stdout.log"
    dump_file "apache error log" "$LOGDIR/error.log"
    dump_file "apache access log" "$LOGDIR/access.log"
    printf '=== APACHE_PHASE20_DIAG_END ===\n'
}

cleanup() {
    if [ -n "$HTTPD_PID" ] && kill -0 "$HTTPD_PID" 2>/dev/null; then
        kill -TERM "$HTTPD_PID" 2>/dev/null || true
        i=0
        while kill -0 "$HTTPD_PID" 2>/dev/null && [ "$i" -lt 10 ]; do
            apache_runner_sleep 1
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
    printf 'phase20 index\n' > "$DOCROOT/index.html"
    printf 'phase20 small\n' > "$DOCROOT/small.txt"

    cat > "$CONF" <<EOF
Include /etc/apache2/httpd.conf
ServerName 127.0.0.1
PidFile $RUNDIR/httpd.pid
Mutex fcntl:$RUNDIR mpm-accept
Listen 127.0.0.1:8080
ErrorLog $LOGDIR/error.log
CustomLog $LOGDIR/access.log common
ExtendedStatus On
StartServers 1
MinSpareServers 1
MaxSpareServers 1
ServerLimit 2
MaxRequestWorkers 2

<VirtualHost 127.0.0.1:8080>
    ServerName localhost
    DocumentRoot "$DOCROOT"
    ErrorLog "$LOGDIR/error.log"
    CustomLog "$LOGDIR/access.log" common
    <Location /server-status>
        SetHandler server-status
        Require all granted
    </Location>
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
    httpd -t -f "$CONF" || return 1
    httpd -k start -f "$CONF" > "$LOGDIR/httpd-stdout.log" 2>&1 || return 1
    i=0
    while [ "$i" -lt 30 ]; do
        if [ -f "$RUNDIR/httpd.pid" ]; then
            HTTPD_PID=$(cat "$RUNDIR/httpd.pid")
            if kill -0 "$HTTPD_PID" 2>/dev/null; then
                if apache_runner_run_with_timeout 2 curl -fsS -o "$OUT/startup.body" http://127.0.0.1:8080/ >/dev/null 2>&1; then return 0; fi
            fi
        fi
        apache_runner_sleep 1
        i=$((i + 1))
    done
    return 1
}

test_worker_pool_ready() {
    i=0
    while [ "$i" -lt 15 ]; do
        if apache_runner_run_with_timeout 5 curl -fsS -o "$OUT/server-status.auto" "http://127.0.0.1:8080/server-status?auto" >/dev/null 2>&1; then
            if grep -qi '^ServerMPM: prefork' "$OUT/server-status.auto"; then
                return 0
            fi
        fi
        apache_runner_sleep 1
        i=$((i + 1))
    done
    return 1
}

test_request_handling() {
    apache_runner_run_with_timeout 5 curl -fsS -o "$OUT/index.body" http://127.0.0.1:8080/
    grep -qx 'phase20 index' "$OUT/index.body"
    apache_runner_run_with_timeout 5 curl -fsS -o "$OUT/small.body" http://127.0.0.1:8080/small.txt
    grep -qx 'phase20 small' "$OUT/small.body"
}

test_stop_cleanup() {
    httpd -k stop -f "$CONF" >/dev/null 2>&1 || kill -TERM "$HTTPD_PID"
    i=0
    while kill -0 "$HTTPD_PID" 2>/dev/null && [ "$i" -lt 10 ]; do
        apache_runner_sleep 1
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
run_step "start apache daemon" start_httpd
run_step "worker pool ready" test_worker_pool_ready
run_step "request handling" test_request_handling
run_step "stop cleanup" test_stop_cleanup
printf 'APACHE_PHASE20_TEST_PASSED\n'
