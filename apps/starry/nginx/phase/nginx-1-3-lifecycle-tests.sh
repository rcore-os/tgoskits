#!/bin/sh
set -eu

. /usr/bin/nginx-alpine-mirror.sh

BASE=/tmp/nginx-phase1
WWW="$BASE/www"
CONF_DIR="$BASE/conf"
OUT="$BASE/out"
LOG_DIR="$BASE/logs"
TIMEOUT_CMD=

log() { printf 'NGINX_PHASE1_LOG: %s\n' "$*"; }
fail() { printf 'NGINX_PHASE1_TEST_FAILED\n'; log "$*"; exit 1; }

init_timeout_cmd() {
    if command -v timeout >/dev/null 2>&1; then
        TIMEOUT_CMD='timeout'
        return
    fi
    if busybox timeout 2>&1 | grep -qi 'usage'; then
        TIMEOUT_CMD='busybox timeout'
        return
    fi
    fail "timeout command not available"
}

run_with_timeout() {
    sec=$1
    shift
    $TIMEOUT_CMD "$sec" "$@"
}

cleanup_nginx() {
    killall -q nginx 2>/dev/null || true
    sleep 1
    killall -q -9 nginx 2>/dev/null || true
}

prepare_packages() {
    nginx_apk_add_with_fallback nginx curl busybox-extras procps || return 1
}

prepare_files() {
    rm -rf "$BASE"
    mkdir -p "$WWW" "$CONF_DIR" "$OUT" "$LOG_DIR"
    printf 'PHASE1_OK\n' > "$WWW/index.html"
    cat > "$CONF_DIR/single.conf" <<'EOF'
daemon off;
master_process off;
worker_processes 1;
error_log /tmp/nginx-phase1/logs/error-single.log debug;
pid /tmp/nginx-phase1/nginx-single.pid;
events { worker_connections 64; }
http { include /etc/nginx/mime.types; access_log /tmp/nginx-phase1/logs/access-single.log; server { listen 127.0.0.1:8080; root /tmp/nginx-phase1/www; location / { index index.html; } } }
EOF
    cat > "$CONF_DIR/master1.conf" <<'EOF'
daemon off;
master_process on;
worker_processes 1;
error_log /tmp/nginx-phase1/logs/error-master1.log debug;
pid /tmp/nginx-phase1/nginx-master1.pid;
events { worker_connections 64; }
http { include /etc/nginx/mime.types; access_log /tmp/nginx-phase1/logs/access-master1.log; server { listen 127.0.0.1:8081; root /tmp/nginx-phase1/www; location / { index index.html; } } }
EOF
    cat > "$CONF_DIR/master2.conf" <<'EOF'
daemon off;
master_process on;
worker_processes 2;
error_log /tmp/nginx-phase1/logs/error-master2.log debug;
pid /tmp/nginx-phase1/nginx-master2.pid;
events { worker_connections 128; }
http { include /etc/nginx/mime.types; access_log /tmp/nginx-phase1/logs/access-master2.log; server { listen 127.0.0.1:8082; root /tmp/nginx-phase1/www; location / { index index.html; } } }
EOF
}

wait_http_ok() {
    url=$1
    i=0
    while [ "$i" -lt 6 ]; do
        if run_with_timeout 1 curl -fsS "$url" -o "$OUT/tmp.body" >/dev/null 2>&1; then
            return 0
        fi
        sleep 1
        i=$((i + 1))
    done
    return 1
}

test_master2() {
    nginx -t -c "$CONF_DIR/master2.conf" -p "$BASE/" || return 1
    nginx -c "$CONF_DIR/master2.conf" -p "$BASE/" > "$LOG_DIR/master2.stdout" 2>&1 &
    wait_http_ok http://127.0.0.1:8082/ || return 1
    if command -v pgrep >/dev/null 2>&1; then
        workers=$(pgrep -xc nginx)
    else
        workers=$(ps | grep '/usr/sbin/nginx\| nginx$' | grep -v grep | wc -l)
    fi
    log "phase1.3 nginx_proc_count=$workers"
    [ "$workers" -ge 3 ] || return 1
    i=1
    while [ "$i" -le 3 ]; do
        run_with_timeout 1 curl -fsS http://127.0.0.1:8082/ -o "$OUT/m2-$i.body" >/dev/null 2>&1 || return 1
        i=$((i + 1))
    done
    run_with_timeout 2 nginx -s quit -c "$CONF_DIR/master2.conf" -p "$BASE/" >/dev/null 2>&1 || return 1
    return 0
}

init_timeout_cmd
( sleep 90; log "watchdog timeout"; kill -TERM $$ ) &
prepare_packages || fail "prepare packages"
prepare_files || fail "prepare files"
nginx -t -c "$CONF_DIR/single.conf" -p "$BASE/" || fail "single config"
nginx -c "$CONF_DIR/single.conf" -p "$BASE/" > "$LOG_DIR/single.stdout" 2>&1 &
wait_http_ok http://127.0.0.1:8080/ || fail "phase1.1"
cleanup_nginx
nginx -t -c "$CONF_DIR/master1.conf" -p "$BASE/" || fail "master1 config"
nginx -c "$CONF_DIR/master1.conf" -p "$BASE/" > "$LOG_DIR/master1.stdout" 2>&1 &
wait_http_ok http://127.0.0.1:8081/ || fail "phase1.2"
run_with_timeout 2 nginx -s quit -c "$CONF_DIR/master1.conf" -p "$BASE/" >/dev/null 2>&1 || fail "master1 quit"
test_master2 || fail "phase1.3"
cleanup_nginx
printf 'NGINX_PHASE1_TEST_PASSED\n'
