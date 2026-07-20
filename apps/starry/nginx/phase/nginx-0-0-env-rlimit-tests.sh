#!/bin/sh
set -eu

. /usr/bin/nginx-runner-lib.sh

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
    runner_ensure_packages || return 1
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
# Defensive cleanup: this phase only runs `nginx -t` (no daemon), but reap any
# stray nginx on exit so a failure mid-run cannot leak a process. Timeout is
# owned by the runner, so no in-script watchdog here.
trap 'killall -q nginx 2>/dev/null || true' EXIT INT TERM
prepare_packages || fail "prepare packages"
prepare_tree || fail "prepare tree"
test_rlimit_probe || fail "getrlimit/setrlimit probe"
printf 'NGINX_PHASE00_TEST_PASSED\n'
