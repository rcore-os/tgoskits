#!/bin/sh
set -eu

BASE=/tmp/nginx-phase00
CONF="$BASE/conf/rlimit.conf"
WWW="$BASE/www"
LOGDIR="$BASE/logs"
TIMEOUT_CMD=

log() { printf 'NGINX_PHASE00_LOG: %s\n' "$*"; }
fail() { printf 'NGINX_PHASE00_TEST_FAILED\n'; log "$*"; exit 1; }

init_timeout_cmd() {
    if command -v timeout >/dev/null 2>&1; then TIMEOUT_CMD='timeout'; return; fi
    if busybox timeout 2>&1 | grep -qi 'usage'; then TIMEOUT_CMD='busybox timeout'; return; fi
    fail "timeout command not available"
}

run_with_timeout() { sec=$1; shift; $TIMEOUT_CMD "$sec" "$@"; }

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
    mkdir -p "$BASE/conf" "$WWW" "$LOGDIR"
    printf 'phase00\n' > "$WWW/index.html"
    cat > "$CONF" <<'EOF'
daemon off;
master_process off;
worker_processes 1;
error_log /tmp/nginx-phase00/logs/error.log debug;
pid /tmp/nginx-phase00/nginx.pid;
events { worker_connections 64; }
http {
    include /etc/nginx/mime.types;
    access_log /tmp/nginx-phase00/logs/access.log;
    server { listen 127.0.0.1:8080; root /tmp/nginx-phase00/www; }
}
EOF
}

test_rlimit_probe() {
    before=$(ulimit -n)
    [ -n "$before" ]
    ulimit -n 1024
    after=$(ulimit -n)
    [ "$after" = "1024" ] || [ "$after" -ge 1024 ]
    nginx -t -c "$CONF" -p "$BASE/"
    ulimit -n "$before"
    log "rlimit_nofile_before=$before after=$after"
}

init_timeout_cmd
( sleep 60; log "watchdog timeout"; kill -TERM $$ ) &
prepare_packages || fail "prepare packages"
prepare_tree || fail "prepare tree"
test_rlimit_probe || fail "getrlimit/setrlimit probe"
printf 'NGINX_PHASE00_TEST_PASSED\n'
