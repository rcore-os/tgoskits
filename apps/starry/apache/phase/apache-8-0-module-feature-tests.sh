#!/bin/sh

BASE=/tmp/apache-phase80
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
APP_DIR=$(dirname "$SCRIPT_DIR")
CONF="$BASE/conf/module-features.conf"
DOC1="$BASE/htdocs1"
DOC2="$BASE/htdocs2"
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

log() { printf 'APACHE_PHASE80_LOG: %s\n' "$*"; }
fail() { printf 'APACHE_PHASE80_TEST_FAILED\n'; log "$*"; exit 1; }
pass_step() { printf 'APACHE_PHASE80_STEP_PASS: %s\n' "$*"; }

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
    printf '=== APACHE_PHASE80_DIAG_BEGIN ===\n'
    date 2>&1 || true
    uname -a 2>&1 || true
    ps 2>&1 || true
    ls -la "$BASE" "$DOC1" "$DOC2" "$LOGDIR" "$RUNDIR" "$OUT" 2>&1 || true
    dump_file "apache config" "$CONF"
    dump_file "apache stdout" "$LOGDIR/httpd-stdout.log"
    dump_file "apache error log" "$LOGDIR/error.log"
    dump_file "apache access log" "$LOGDIR/access.log"
    printf '=== APACHE_PHASE80_DIAG_END ===\n'
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
    mkdir -p "$BASE/conf" "$DOC1" "$DOC2" "$LOGDIR" "$RUNDIR" "$OUT"
    printf 'phase80 vhost1\n' > "$DOC1/index.html"
    printf 'phase80 vhost2\n' > "$DOC2/index.html"
    printf 'phase80 mime text\n' > "$DOC1/mime.txt"
    printf 'phase80 rewritten\n' > "$DOC1/rewritten.html"
    dd if=/dev/zero bs=1024 count=64 2>/dev/null | tr '\0' 'A' > "$DOC1/gzip.txt"

    cat > "$CONF" <<EOF
Include /etc/apache2/httpd.conf
LoadModule deflate_module modules/mod_deflate.so
LoadModule rewrite_module modules/mod_rewrite.so
ServerName 127.0.0.1
PidFile $RUNDIR/httpd.pid
Mutex fcntl:$RUNDIR mpm-accept
Listen 127.0.0.1:8080
ErrorLog $LOGDIR/error.log
CustomLog $LOGDIR/access.log common
ExtendedStatus On

<Location /server-status>
    SetHandler server-status
    Require all granted
</Location>

<VirtualHost 127.0.0.1:8080>
    ServerName first.local
    DocumentRoot "$DOC1"
    ErrorLog "$LOGDIR/error.log"
    CustomLog "$LOGDIR/access.log" common
    RewriteEngine On
    RewriteCond %{REQUEST_URI} ^/rewrite-me$
    RewriteRule .* /rewritten.html [L]
    <Directory "$DOC1">
        Require all granted
        Options +Indexes +FollowSymLinks
        AllowOverride None
        DirectoryIndex index.html
        AddOutputFilterByType DEFLATE text/plain
    </Directory>
</VirtualHost>

<VirtualHost 127.0.0.1:8080>
    ServerName second.local
    DocumentRoot "$DOC2"
    ErrorLog "$LOGDIR/error.log"
    CustomLog "$LOGDIR/access.log" common
    <Directory "$DOC2">
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
                if run_with_timeout 2 curl -fsS -H 'Host: first.local' -o "$OUT/startup.body" http://127.0.0.1:8080/ >/dev/null 2>&1; then return 0; fi
            fi
        fi
        sleep 1
        i=$((i + 1))
    done
    return 1
}

test_mime_type() {
    run_with_timeout 5 curl -fsS -D "$OUT/mime.headers" -o "$OUT/mime.body" -H 'Host: first.local' http://127.0.0.1:8080/mime.txt
    grep -qi '^Content-Type: text/plain' "$OUT/mime.headers"
    grep -qx 'phase80 mime text' "$OUT/mime.body"
}

test_rewrite() {
    run_with_timeout 5 curl -fsS -D "$OUT/rewrite.headers" -o "$OUT/rewrite.body" -H 'Host: first.local' http://127.0.0.1:8080/rewrite-me
    grep -qx 'phase80 rewritten' "$OUT/rewrite.body"
}

test_deflate() {
    run_with_timeout 5 curl -fsS -D "$OUT/deflate.headers" -o "$OUT/deflate.body" -H 'Host: first.local' -H 'Accept-Encoding: gzip' http://127.0.0.1:8080/gzip.txt
    grep -qi '^Content-Encoding: gzip' "$OUT/deflate.headers"
}

test_status_auto() {
    run_with_timeout 5 curl -fsS -D "$OUT/status.headers" -o "$OUT/status.body" -H 'Host: first.local' http://127.0.0.1:8080/server-status?auto
    grep -qi '^ServerMPM: prefork' "$OUT/status.body"
    grep -qi '^Total Accesses:' "$OUT/status.body"
}

test_name_based_vhost() {
    run_with_timeout 5 curl -fsS -D "$OUT/vhost2.headers" -o "$OUT/vhost2.body" -H 'Host: second.local' http://127.0.0.1:8080/
    grep -qx 'phase80 vhost2' "$OUT/vhost2.body"
}

stop_httpd() {
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
run_step "mime type" test_mime_type
run_step "rewrite" test_rewrite
run_step "deflate" test_deflate
run_step "status auto" test_status_auto
run_step "name based vhost" test_name_based_vhost
run_step "stop apache" stop_httpd
printf 'APACHE_PHASE80_TEST_PASSED\n'
