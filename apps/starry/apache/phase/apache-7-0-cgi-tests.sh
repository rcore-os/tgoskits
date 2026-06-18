#!/bin/sh
set -eu

BASE=/tmp/apache-phase70
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
APP_DIR=$(dirname "$SCRIPT_DIR")
CONF="$BASE/conf/cgi.conf"
DOCROOT="$BASE/htdocs"
CGIBIN="$BASE/cgi-bin"
CGIURL="/phase70-cgi/"
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

log() { printf 'APACHE_PHASE70_LOG: %s\n' "$*"; }
fail() { printf 'APACHE_PHASE70_TEST_FAILED\n'; log "$*"; exit 1; }
pass_step() { printf 'APACHE_PHASE70_STEP_PASS: %s\n' "$*"; }

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
    printf '=== APACHE_PHASE70_DIAG_BEGIN ===\n'
    date 2>&1 || true
    uname -a 2>&1 || true
    ps 2>&1 || true
    ls -la "$BASE" "$DOCROOT" "$CGIBIN" "$LOGDIR" "$RUNDIR" "$OUT" 2>&1 || true
    dump_file "apache config" "$CONF"
    dump_file "apache stdout" "$LOGDIR/httpd-stdout.log"
    dump_file "apache error log" "$LOGDIR/error.log"
    dump_file "apache access log" "$LOGDIR/access.log"
    printf '=== APACHE_PHASE70_DIAG_END ===\n'
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
    mkdir -p "$BASE/conf" "$DOCROOT" "$CGIBIN" "$LOGDIR" "$RUNDIR" "$OUT"
    printf 'phase70 index\n' > "$DOCROOT/index.html"
    printf 'phase70 cgi ok\n' > "$CGIBIN/echo.cgi"
    cat > "$CGIBIN/echo.cgi" <<'EOF_CGI'
#!/bin/sh
printf 'Content-Type: text/plain\r\n'
printf '\r\n'
printf 'REQUEST_METHOD=%s\n' "${REQUEST_METHOD:-}"
printf 'CONTENT_LENGTH=%s\n' "${CONTENT_LENGTH:-}"
printf 'SCRIPT_NAME=%s\n' "${SCRIPT_NAME:-}"
body_bytes=$(cat | wc -c)
printf 'BODY_BYTES=%s\n' "$body_bytes"
EOF_CGI
    chmod 0755 "$CGIBIN/echo.cgi"

    cat > "$CGIBIN/fail.cgi" <<'EOF_FAIL'
#!/bin/sh
exit 42
EOF_FAIL
    chmod 0755 "$CGIBIN/fail.cgi"

    dd if=/dev/zero of="$OUT/post-large.bin" bs=1024 count=4 2>/dev/null
    dd if=/dev/zero of="$OUT/post-over.bin" bs=1024 count=16 2>/dev/null

    cat > "$CONF" <<EOF
Include /etc/apache2/httpd.conf
LoadModule cgi_module modules/mod_cgi.so
ServerName 127.0.0.1
PidFile $RUNDIR/httpd.pid
Mutex fcntl:$RUNDIR mpm-accept
Listen 127.0.0.1:8080
ErrorLog $LOGDIR/error.log
CustomLog $LOGDIR/access.log common
LimitRequestBody 8192
    ScriptAlias $CGIURL "$CGIBIN/"

<Directory "$DOCROOT">
    Require all granted
    Options +Indexes +FollowSymLinks
    AllowOverride None
    DirectoryIndex index.html
</Directory>

<Directory "$CGIBIN">
    Require all granted
    Options +ExecCGI -Indexes
    AllowOverride None
    AddHandler cgi-script .cgi
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
    httpd -k start -f "$CONF" > "$LOGDIR/httpd-stdout.log" 2>&1 || return 1
    i=0
    while [ "$i" -lt 30 ]; do
        if [ -f "$RUNDIR/httpd.pid" ]; then
            HTTPD_PID=$(cat "$RUNDIR/httpd.pid")
            if kill -0 "$HTTPD_PID" 2>/dev/null; then
                if apache_runner_run_with_timeout 2 curl -fsS -o "$OUT/startup.body" http://127.0.0.1:8080/ >/dev/null 2>&1; then return 0; fi
            fi
        fi
        sleep 1
        i=$((i + 1))
    done
    return 1
}

test_cgi_env_and_body() {
    apache_runner_run_with_timeout 5 curl -fsS -D "$OUT/cgi-post.headers" -o "$OUT/cgi-post.body" -X POST --data 'abc123' http://127.0.0.1:8080${CGIURL}echo.cgi
    grep -qi '^Content-Type: text/plain' "$OUT/cgi-post.headers"
    grep -qx 'REQUEST_METHOD=POST' "$OUT/cgi-post.body"
    grep -qx 'CONTENT_LENGTH=6' "$OUT/cgi-post.body"
    grep -qx "SCRIPT_NAME=${CGIURL}echo.cgi" "$OUT/cgi-post.body"
    grep -qx 'BODY_BYTES=6' "$OUT/cgi-post.body"
}

test_cgi_get_env() {
    apache_runner_run_with_timeout 5 curl -fsS -D "$OUT/cgi-get.headers" -o "$OUT/cgi-get.body" http://127.0.0.1:8080${CGIURL}echo.cgi
    grep -qx 'REQUEST_METHOD=GET' "$OUT/cgi-get.body"
    grep -qx 'CONTENT_LENGTH=' "$OUT/cgi-get.body"
    grep -qx 'BODY_BYTES=0' "$OUT/cgi-get.body"
}

test_cgi_large_body() {
    apache_runner_run_with_timeout 8 curl -fsS -D "$OUT/cgi-large.headers" -o "$OUT/cgi-large.body" -X POST --data-binary "@$OUT/post-large.bin" http://127.0.0.1:8080${CGIURL}echo.cgi
    grep -qx 'BODY_BYTES=4096' "$OUT/cgi-large.body"
}

test_limit_request_body_413() {
    code=$(apache_runner_run_with_timeout 8 curl -sS -D "$OUT/cgi-over.headers" -o "$OUT/cgi-over.body" -w '%{http_code}' -X POST --data-binary "@$OUT/post-over.bin" http://127.0.0.1:8080${CGIURL}echo.cgi || printf 'curl_failed')
    [ "$code" = "413" ]
}

test_cgi_fail_500() {
    code=$(apache_runner_run_with_timeout 5 curl -sS -D "$OUT/cgi-fail.headers" -o "$OUT/cgi-fail.body" -w '%{http_code}' http://127.0.0.1:8080${CGIURL}fail.cgi || printf 'curl_failed')
    [ "$code" = "500" ]
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

apache_runner_init_timeout_cmd || fail "timeout command not available"
run_step "prepare packages" prepare_packages
run_step "prepare apache files" prepare_tree
run_step "start apache" start_httpd
run_step "cgi env and body" test_cgi_env_and_body
run_step "cgi get env" test_cgi_get_env
run_step "cgi large body" test_cgi_large_body
run_step "limit request body 413" test_limit_request_body_413
run_step "cgi fail 500" test_cgi_fail_500
run_step "stop apache" stop_httpd
printf 'APACHE_PHASE70_TEST_PASSED\n'
