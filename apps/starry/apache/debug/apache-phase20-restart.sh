#!/bin/sh
set -eu

BASE=/tmp/apache-phase20-debug
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

log() { printf 'APACHE_PHASE20_RESTART_DEBUG_LOG: %s\n' "$*"; }
fail() { printf 'APACHE_PHASE20_RESTART_DEBUG_FAILED\n'; log "$*"; exit 1; }

dump_file() {
    name=$1
    path=$2
    printf -- '--- %s: %s ---\n' "$name" "$path"
    if [ -f "$path" ]; then
        sed -n '1,220p' "$path" 2>&1
    else
        printf 'missing\n'
    fi
}

dump_state() {
    printf '=== APACHE_PHASE20_RESTART_DEBUG_STATE_BEGIN ===\n'
    date 2>&1 || true
    ps 2>&1 || true
    if command -v pgrep >/dev/null 2>&1; then
        pgrep -af httpd 2>/dev/null || true
    fi
    dump_file "apache stdout" "$LOGDIR/httpd-stdout.log"
    dump_file "apache error log" "$LOGDIR/error.log"
    dump_file "apache access log" "$LOGDIR/access.log"
    dump_file "server-status before" "$OUT/server-status.before"
    dump_file "server-status after" "$OUT/server-status.after"
    printf '=== APACHE_PHASE20_RESTART_DEBUG_STATE_END ===\n'
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
        dump_state
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
    printf 'phase20 debug index\n' > "$DOCROOT/index.html"
    printf 'phase20 debug small\n' > "$DOCROOT/small.txt"

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
                if apache_runner_run_with_timeout 2 curl -fsS -o "$OUT/startup.body" http://127.0.0.1:8080/ >/dev/null 2>&1; then
                    return 0
                fi
            fi
        fi
        apache_runner_sleep 1
        i=$((i + 1))
    done
    return 1
}

restart_probe() {
    apache_runner_run_with_timeout 5 curl -fsS -o "$OUT/server-status.before" "http://127.0.0.1:8080/server-status?auto" || return 1
    grep -qi '^ServerMPM: prefork' "$OUT/server-status.before" || return 1
    log "before_restart_pid=$HTTPD_PID"
    kill -HUP "$HTTPD_PID" || return 1

    i=0
    while [ "$i" -lt 15 ]; do
        if kill -0 "$HTTPD_PID" 2>/dev/null && apache_runner_run_with_timeout 5 curl -fsS -o "$OUT/server-status.after" "http://127.0.0.1:8080/server-status?auto" >/dev/null 2>&1; then
            break
        fi
        apache_runner_sleep 1
        i=$((i + 1))
    done
    [ -f "$OUT/server-status.after" ] || return 1
    grep -qi '^ServerMPM: prefork' "$OUT/server-status.after" || return 1
    apache_runner_run_with_timeout 5 curl -fsS -o "$OUT/after-restart.body" http://127.0.0.1:8080/small.txt || return 1
    grep -qx 'phase20 debug small' "$OUT/after-restart.body" || return 1
    log "after_restart_pid=$HTTPD_PID"
    return 0
}

apache_runner_init_timeout_cmd || fail "timeout command not available"
prepare_packages || fail "prepare packages"
prepare_tree || fail "prepare tree"
start_httpd || fail "start apache"
restart_probe || fail "restart probe"
printf 'APACHE_PHASE20_RESTART_DEBUG_PASSED\n'
