#!/bin/sh
set -eu

BASE=/tmp/nginx-phase90
CONF="$BASE/conf/config-feature.conf"
CONF_IPV6="$BASE/conf/ipv6.conf"
CONF_UNIX="$BASE/conf/unix.conf"
WWW="$BASE/www"
OUT="$BASE/out"
LOGDIR="$BASE/logs"
TIMEOUT_CMD=

log() { printf 'NGINX_PHASE90_LOG: %s\n' "$*"; }
fail() { printf 'NGINX_PHASE90_TEST_FAILED\n'; log "$*"; exit 1; }

init_timeout_cmd() {
    if command -v timeout >/dev/null 2>&1; then TIMEOUT_CMD='timeout'; return; fi
    if busybox timeout 2>&1 | grep -qi 'usage'; then TIMEOUT_CMD='busybox timeout'; return; fi
    fail "timeout command not available"
}

run_with_timeout() { sec=$1; shift; $TIMEOUT_CMD "$sec" "$@"; }

cleanup_nginx() {
    killall -q nginx 2>/dev/null || true
    sleep 1
    killall -q -9 nginx 2>/dev/null || true
}

prepare_packages() {
    repo_file=/etc/apk/repositories
    original_repos="$(cat "$repo_file")"
    for mirror in https://mirrors.cernet.edu.cn/alpine https://dl-cdn.alpinelinux.org/alpine; do
        printf '%s\n' "$original_repos" | sed "s#http://[^/]*/alpine/#$mirror/#g;s#https://[^/]*/alpine/#$mirror/#g" > "$repo_file"
        rm -f /lib/apk/db/lock
        if run_with_timeout 40 apk --timeout 40 update && run_with_timeout 40 apk --timeout 40 add nginx curl busybox-extras; then return 0; fi
    done
    return 1
}

prepare_tree() {
    rm -rf "$BASE"
    mkdir -p "$BASE/conf" "$WWW/dir1" "$WWW/dir2" "$WWW/errors" "$WWW/alias-src" "$OUT" "$LOGDIR"
    printf 'phase90 index\n' > "$WWW/index.html"
    printf 'auto file\n' > "$WWW/dir1/a.txt"
    printf 'try files ok\n' > "$WWW/dir2/fallback.txt"
    printf 'custom 404 page\n' > "$WWW/errors/404.html"
    printf 'alias data\n' > "$WWW/alias-src/file.txt"
    # bigger text to make gzip behavior visible.
    dd if=/dev/zero bs=1024 count=64 2>/dev/null | tr '\0' 'A' > "$WWW/large.txt"
    cat > "$CONF" <<'EOF'
daemon off;
master_process off;
worker_processes 1;
error_log /tmp/nginx-phase90/logs/error.log debug;
pid /tmp/nginx-phase90/nginx.pid;
events { worker_connections 64; }
http {
    include /etc/nginx/mime.types;
    default_type text/plain;
    access_log /tmp/nginx-phase90/logs/access.log;

    server {
        listen 127.0.0.1:8080;
        root /tmp/nginx-phase90/www;

        location / { index index.html; }

        location /auto/ {
            alias /tmp/nginx-phase90/www/dir1/;
            autoindex on;
        }

        location /try {
            try_files /no-file /dir2/fallback.txt =404;
        }

        error_page 404 /errors/404.html;

        location /alias/ {
            alias /tmp/nginx-phase90/www/alias-src/;
        }

        location /gzip-off {
            gzip off;
            try_files /large.txt =404;
        }

        location /gzip-on {
            gzip on;
            gzip_min_length 1;
            gzip_types text/plain;
            try_files /large.txt =404;
        }
    }
}
EOF

    cat > "$CONF_IPV6" <<'EOF'
daemon off;
master_process off;
worker_processes 1;
error_log /tmp/nginx-phase90/logs/error-ipv6.log debug;
pid /tmp/nginx-phase90/nginx-ipv6.pid;
events { worker_connections 64; }
http {
    include /etc/nginx/mime.types;
    access_log /tmp/nginx-phase90/logs/access-ipv6.log;
    server {
        listen [::1]:8081;
        root /tmp/nginx-phase90/www;
        location / { index index.html; }
    }
}
EOF

    cat > "$CONF_UNIX" <<'EOF'
daemon off;
master_process off;
worker_processes 1;
error_log /tmp/nginx-phase90/logs/error-unix.log debug;
pid /tmp/nginx-phase90/nginx-unix.pid;
events { worker_connections 64; }
http {
    include /etc/nginx/mime.types;
    access_log /tmp/nginx-phase90/logs/access-unix.log;
    server {
        listen unix:/tmp/nginx-phase90/nginx.sock;
        root /tmp/nginx-phase90/www;
        location / { index index.html; }
    }
}
EOF
}

start_nginx() {
    nginx -t -c "$CONF" -p "$BASE/" || return 1
    nginx -c "$CONF" -p "$BASE/" > "$LOGDIR/nginx-stdout.log" 2>&1 &
    i=0
    while [ "$i" -lt 8 ]; do
        run_with_timeout 1 curl -fsS -o /dev/null http://127.0.0.1:8080/ >/dev/null 2>&1 && return 0
        i=$((i + 1))
        sleep 1
    done
    return 1
}

test_autoindex() {
    run_with_timeout 6 curl -fsS -o "$OUT/auto.body" http://127.0.0.1:8080/auto/
    grep -q 'a.txt' "$OUT/auto.body"
}

test_try_files() {
    run_with_timeout 6 curl -fsS -o "$OUT/try.body" http://127.0.0.1:8080/try
    grep -qx 'try files ok' "$OUT/try.body"
}

test_error_page() {
    code=$(run_with_timeout 6 curl -sS -o "$OUT/error-page.body" -w '%{http_code}' http://127.0.0.1:8080/definitely-missing || true)
    [ "$code" = "404" ]
    grep -qx 'custom 404 page' "$OUT/error-page.body"
}

test_alias() {
    run_with_timeout 6 curl -fsS -o "$OUT/alias.body" http://127.0.0.1:8080/alias/file.txt
    grep -qx 'alias data' "$OUT/alias.body"
}

test_gzip_off_on() {
    run_with_timeout 8 curl -fsS -D "$OUT/gzip-off.h" -H 'Accept-Encoding: gzip' -o /dev/null http://127.0.0.1:8080/gzip-off
    ! grep -qi '^Content-Encoding: gzip' "$OUT/gzip-off.h"

    run_with_timeout 8 curl -fsS -D "$OUT/gzip-on.h" -H 'Accept-Encoding: gzip' -o /dev/null http://127.0.0.1:8080/gzip-on
    grep -qi '^Content-Encoding: gzip' "$OUT/gzip-on.h"
}

test_ipv6_listen() {
    cleanup_nginx
    nginx -t -c "$CONF_IPV6" -p "$BASE/" || return 1
    nginx -c "$CONF_IPV6" -p "$BASE/" > "$LOGDIR/nginx-ipv6-stdout.log" 2>&1 &
    i=0
    while [ "$i" -lt 6 ]; do
        if run_with_timeout 2 curl -g -6 -fsS -o "$OUT/ipv6.body" http://[::1]:8081/ >/dev/null 2>&1; then
            break
        fi
        i=$((i + 1))
        sleep 1
    done
    [ "$i" -lt 6 ] || return 1
    grep -qx 'phase90 index' "$OUT/ipv6.body"
}

test_unix_socket_listen() {
    cleanup_nginx
    rm -f /tmp/nginx-phase90/nginx.sock
    nginx -t -c "$CONF_UNIX" -p "$BASE/" || return 1
    nginx -c "$CONF_UNIX" -p "$BASE/" > "$LOGDIR/nginx-unix-stdout.log" 2>&1 &
    i=0
    while [ "$i" -lt 6 ]; do
        if run_with_timeout 2 curl --unix-socket /tmp/nginx-phase90/nginx.sock -fsS -o "$OUT/unix.body" http://localhost/ >/dev/null 2>&1; then
            break
        fi
        i=$((i + 1))
        sleep 1
    done
    [ "$i" -lt 6 ] || return 1
    grep -qx 'phase90 index' "$OUT/unix.body"
}

init_timeout_cmd
( sleep 120; log "watchdog timeout"; kill -TERM $$ ) &
prepare_packages || fail "prepare packages"
prepare_tree || fail "prepare tree"
start_nginx || fail "start nginx"
test_autoindex || fail "autoindex on"
test_try_files || fail "try_files"
test_error_page || fail "error_page"
test_alias || fail "alias"
test_gzip_off_on || fail "gzip off/on"
test_ipv6_listen || fail "ipv6 listen"
test_unix_socket_listen || fail "unix domain socket listen"
cleanup_nginx
printf 'NGINX_PHASE90_TEST_PASSED\n'
