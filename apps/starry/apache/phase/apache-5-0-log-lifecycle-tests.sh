#!/bin/sh

BASE=/tmp/apache-phase50
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
APP_DIR=$(dirname "$SCRIPT_DIR")
CONF="$BASE/conf/log-lifecycle.conf"
DOCROOT="$BASE/htdocs"
LOGDIR="$BASE/logs"
RUNDIR="$BASE/run"
OUT="$BASE/out"
HTTPD_PID=
WATCHDOG_PID=
TIMEOUT_CMD=

if [ -f /usr/bin/apache-alpine-mirror.sh ]; then
    . /usr/bin/apache-alpine-mirror.sh
elif [ -f "$APP_DIR/apache-alpine-mirror.sh" ]; then
    . "$APP_DIR/apache-alpine-mirror.sh"
fi

log() { printf 'APACHE_PHASE50_LOG: %s\n' "$*"; }
fail() { printf 'APACHE_PHASE50_TEST_FAILED\n'; log "$*"; exit 1; }
pass_step() { printf 'APACHE_PHASE50_STEP_PASS: %s\n' "$*"; }

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
    printf '=== APACHE_PHASE50_DIAG_BEGIN ===\n'
    date 2>&1 || true
    uname -a 2>&1 || true
    ps 2>&1 || true
    ls -la "$BASE" "$DOCROOT" "$LOGDIR" "$RUNDIR" "$OUT" 2>&1 || true
    dump_file "apache config" "$CONF"
    dump_file "apache stdout" "$LOGDIR/httpd-stdout.log"
    dump_file "apache error log" "$LOGDIR/error.log"
    dump_file "apache access log" "$LOGDIR/access.log"
    dump_file "apache access old log" "$LOGDIR/access.log.old"
    printf '=== APACHE_PHASE50_DIAG_END ===\n'
}

cleanup() {
    if [ -n "$WATCHDOG_PID" ]; then
        kill "$WATCHDOG_PID" 2>/dev/null || true
    fi
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

init_timeout_cmd() {
    if command -v timeout >/dev/null 2>&1; then TIMEOUT_CMD='timeout'; return 0; fi
    if busybox timeout 2>&1 | grep -qi 'usage'; then TIMEOUT_CMD='busybox timeout'; return 0; fi
    fail "timeout command not available"
}

run_with_timeout() {
    sec=$1
    shift
    $TIMEOUT_CMD "$sec" "$@"
}

prepare_packages() {
    if command -v httpd >/dev/null 2>&1 && command -v curl >/dev/null 2>&1; then
        return 0
    fi
    if command -v apache_apk_add_with_fallback >/dev/null 2>&1; then
        apache_apk_add_with_fallback apache2 apache2-utils curl busybox-extras coreutils
        return $?
    fi
    return 1
}

prepare_tree() {
    rm -rf "$BASE"
    mkdir -p "$BASE/conf" "$DOCROOT" "$LOGDIR" "$RUNDIR" "$OUT"
    printf 'phase50 index\n' > "$DOCROOT/index.html"
    printf 'phase50 small\n' > "$DOCROOT/small.txt"
    : > "$DOCROOT/empty.txt"

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
                if run_with_timeout 2 curl -fsS -o "$OUT/startup.body" http://127.0.0.1:8080/ >/dev/null 2>&1; then return 0; fi
            fi
        fi
        sleep 1
        i=$((i + 1))
    done
    return 1
}

count_access_lines() {
    if [ -f "$LOGDIR/access.log" ]; then
        wc -l < "$LOGDIR/access.log"
    else
        printf '0'
    fi
}

test_access_log_growth() {
    before=$(count_access_lines)
    run_with_timeout 5 curl -fsS -o "$OUT/growth-1.body" http://127.0.0.1:8080/small.txt
    mid=$(count_access_lines)
    run_with_timeout 5 curl -fsS -o "$OUT/growth-2.body" http://127.0.0.1:8080/empty.txt
    after=$(count_access_lines)
    [ "$mid" -gt "$before" ]
    [ "$after" -gt "$mid" ]
}

test_pid_file_present() {
    [ -f "$RUNDIR/httpd.pid" ]
    test "$(cat "$RUNDIR/httpd.pid")" = "$HTTPD_PID"
    kill -0 "$HTTPD_PID"
}

test_graceful_reopen() {
    mv "$LOGDIR/access.log" "$LOGDIR/access.log.old"
    kill -USR1 "$HTTPD_PID"
    i=0
    while [ "$i" -lt 15 ]; do
        if [ -f "$LOGDIR/access.log" ]; then
            break
        fi
        sleep 1
        i=$((i + 1))
    done
    [ -f "$LOGDIR/access.log" ]
    run_with_timeout 5 curl -fsS -o "$OUT/reopen.body" http://127.0.0.1:8080/small.txt
    new_lines=$(wc -l < "$LOGDIR/access.log")
    old_lines=$(wc -l < "$LOGDIR/access.log.old")
    [ "$new_lines" -ge 1 ]
    [ "$old_lines" -ge 1 ]
}

test_restart_works() {
    kill -HUP "$HTTPD_PID"
    i=0
    while [ "$i" -lt 15 ]; do
        if kill -0 "$HTTPD_PID" 2>/dev/null && run_with_timeout 2 curl -fsS -o "$OUT/restart.body" http://127.0.0.1:8080/ >/dev/null 2>&1; then
            return 0
        fi
        sleep 1
        i=$((i + 1))
    done
    return 1
}

test_stop_works() {
    httpd -k stop -f "$CONF" >/dev/null 2>&1 || kill -TERM "$HTTPD_PID"
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

init_timeout_cmd
( sleep 180; log "watchdog timeout"; kill -TERM $$ ) &
WATCHDOG_PID=$!
run_step "prepare packages" prepare_packages
run_step "prepare apache files" prepare_tree
run_step "start apache" start_httpd
run_step "pid file present" test_pid_file_present
run_step "access log growth" test_access_log_growth
run_step "graceful reopen" test_graceful_reopen
run_step "restart works" test_restart_works
run_step "stop works" test_stop_works
printf 'APACHE_PHASE50_TEST_PASSED\n'
