#!/bin/sh

BASE=/tmp/apache-phase40
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
APP_DIR=$(dirname "$SCRIPT_DIR")
CONF="$BASE/conf/directory-access.conf"
DOCROOT="$BASE/htdocs"
ALIASROOT="$BASE/alias-src"
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

log() { printf 'APACHE_PHASE40_LOG: %s\n' "$*"; }
fail() { printf 'APACHE_PHASE40_TEST_FAILED\n'; log "$*"; exit 1; }
pass_step() { printf 'APACHE_PHASE40_STEP_PASS: %s\n' "$*"; }

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
    printf '=== APACHE_PHASE40_DIAG_BEGIN ===\n'
    date 2>&1 || true
    uname -a 2>&1 || true
    ps 2>&1 || true
    ls -la "$BASE" "$DOCROOT" "$ALIASROOT" "$LOGDIR" "$OUT" 2>&1 || true
    dump_file "apache config" "$CONF"
    dump_file "apache stdout" "$LOGDIR/httpd-stdout.log"
    dump_file "apache error log" "$LOGDIR/error.log"
    dump_file "apache access log" "$LOGDIR/access.log"
    printf '=== APACHE_PHASE40_DIAG_END ===\n'
}

cleanup() {
    if [ -n "$WATCHDOG_PID" ]; then
        kill "$WATCHDOG_PID" 2>/dev/null || true
    fi
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
    mkdir -p "$BASE/conf" "$DOCROOT/auto" "$DOCROOT/noindex" "$DOCROOT/errors" "$DOCROOT/denied" "$DOCROOT/ht" "$DOCROOT/symlink-on" "$DOCROOT/symlink-off" "$ALIASROOT" "$LOGDIR" "$RUNDIR" "$OUT"
    printf 'phase40 index\n' > "$DOCROOT/index.html"
    printf 'autoindex file\n' > "$DOCROOT/auto/a.txt"
    printf 'custom 404 page\n' > "$DOCROOT/errors/404.html"
    printf 'denied data\n' > "$DOCROOT/denied/secret.txt"
    printf 'htaccess directory index\n' > "$DOCROOT/ht/htindex.html"
    printf 'DirectoryIndex htindex.html\n' > "$DOCROOT/ht/.htaccess"
    printf 'alias data\n' > "$ALIASROOT/file.txt"
    printf 'symlink target\n' > "$BASE/symlink-target.txt"
    ln -s "$BASE/symlink-target.txt" "$DOCROOT/symlink-on/link.txt"
    ln -s "$BASE/symlink-target.txt" "$DOCROOT/symlink-off/link.txt"

    cat > "$CONF" <<EOF
Include /etc/apache2/httpd.conf
ServerName 127.0.0.1
PidFile $RUNDIR/httpd.pid
Mutex fcntl:$RUNDIR mpm-accept
Listen 127.0.0.1:8080
ErrorLog $LOGDIR/error.log
CustomLog $LOGDIR/access.log common
Alias /alias/ "$ALIASROOT/"
ErrorDocument 404 /errors/404.html

<Directory "$DOCROOT">
    Require all granted
    Options FollowSymLinks
    AllowOverride None
</Directory>

<Directory "$DOCROOT/auto">
    Require all granted
    Options +Indexes +FollowSymLinks
    AllowOverride None
</Directory>

<Directory "$DOCROOT/noindex">
    Require all granted
    Options -Indexes +FollowSymLinks
    AllowOverride None
</Directory>

<Directory "$DOCROOT/denied">
    Require all denied
    Options FollowSymLinks
    AllowOverride None
</Directory>

<Directory "$DOCROOT/ht">
    Require all granted
    Options +Indexes +FollowSymLinks
    AllowOverride Indexes
</Directory>

<Directory "$DOCROOT/symlink-off">
    Require all granted
    Options -FollowSymLinks
    AllowOverride None
</Directory>

<Directory "$ALIASROOT">
    Require all granted
    Options FollowSymLinks
    AllowOverride None
</Directory>

<VirtualHost 127.0.0.1:8080>
    ServerName localhost
    DocumentRoot "$DOCROOT"
    ErrorLog "$LOGDIR/error.log"
    CustomLog "$LOGDIR/access.log" common
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
        if run_with_timeout 2 curl -fsS -o "$OUT/startup.body" http://127.0.0.1:8080/ >/dev/null 2>&1; then return 0; fi
        sleep 1
        i=$((i + 1))
    done
    return 1
}

test_autoindex() {
    run_with_timeout 5 curl -fsS -D "$OUT/auto.headers" -o "$OUT/auto.body" http://127.0.0.1:8080/auto/
    grep -q 'a.txt' "$OUT/auto.body"
}

test_noindex_forbidden() {
    code=$(run_with_timeout 5 curl -sS -o "$OUT/noindex.body" -D "$OUT/noindex.headers" -w '%{http_code}' http://127.0.0.1:8080/noindex/ || printf 'curl_failed')
    [ "$code" = "403" ]
}

test_alias() {
    run_with_timeout 5 curl -fsS -D "$OUT/alias.headers" -o "$OUT/alias.body" http://127.0.0.1:8080/alias/file.txt
    grep -qx 'alias data' "$OUT/alias.body"
}

test_error_document() {
    code=$(run_with_timeout 5 curl -sS -o "$OUT/error-document.body" -D "$OUT/error-document.headers" -w '%{http_code}' http://127.0.0.1:8080/not-here || printf 'curl_failed')
    [ "$code" = "404" ]
    grep -qx 'custom 404 page' "$OUT/error-document.body"
}

test_require_denied() {
    code=$(run_with_timeout 5 curl -sS -o "$OUT/denied.body" -D "$OUT/denied.headers" -w '%{http_code}' http://127.0.0.1:8080/denied/secret.txt || printf 'curl_failed')
    [ "$code" = "403" ]
}

test_htaccess_directory_index() {
    run_with_timeout 5 curl -fsS -D "$OUT/ht.headers" -o "$OUT/ht.body" http://127.0.0.1:8080/ht/
    grep -qx 'htaccess directory index' "$OUT/ht.body"
}

test_symlink_follow_on() {
    run_with_timeout 5 curl -fsS -D "$OUT/symlink-on.headers" -o "$OUT/symlink-on.body" http://127.0.0.1:8080/symlink-on/link.txt
    grep -qx 'symlink target' "$OUT/symlink-on.body"
}

test_symlink_follow_off() {
    code=$(run_with_timeout 5 curl -sS -o "$OUT/symlink-off.body" -D "$OUT/symlink-off.headers" -w '%{http_code}' http://127.0.0.1:8080/symlink-off/link.txt || printf 'curl_failed')
    [ "$code" = "403" ]
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

init_timeout_cmd
( sleep 180; log "watchdog timeout"; kill -TERM $$ ) &
WATCHDOG_PID=$!
run_step "prepare packages" prepare_packages
run_step "prepare apache files" prepare_tree
run_step "start apache" start_httpd
run_step "autoindex" test_autoindex
run_step "noindex forbidden" test_noindex_forbidden
run_step "alias" test_alias
run_step "error document" test_error_document
run_step "require denied" test_require_denied
run_step "htaccess directory index" test_htaccess_directory_index
run_step "symlink follow on" test_symlink_follow_on
run_step "symlink follow off" test_symlink_follow_off
run_step "stop apache" stop_httpd
printf 'APACHE_PHASE40_TEST_PASSED\n'
