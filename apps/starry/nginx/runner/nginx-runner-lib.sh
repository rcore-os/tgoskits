#!/bin/sh
# nginx-runner-lib.sh - shared helpers for the unified nginx test runner.
#
# Sourced by /usr/bin/nginx-runner.sh inside the guest. Provides:
#   - structured marker logging
#   - timeout command detection / wrapper
#   - per-phase isolation (kill residual nginx, free ports, drop temp dirs)
#
# This library is the single place that owns cross-phase isolation, so the
# individual phase scripts no longer need their own watchdog (the old
# `( sleep N; kill -TERM $$ ) &` pattern leaked background jobs that woke up
# during later phases and killed an unrelated, PID-reused process).

# Ports every phase/smoke config binds on 127.0.0.1. Kept in one place so the
# port-release wait stays in sync with the configs.
NGINX_RUNNER_PORTS="8080 8081 8082"

runner_log() { printf 'NGINX_RUNNER_LOG: %s\n' "$*"; }

# Detect a usable `timeout` implementation once and cache it in
# NGINX_RUNNER_TIMEOUT_CMD. Mirrors the probe the phase scripts used, but lives
# in the runner so the timeout responsibility sits at a single layer.
NGINX_RUNNER_TIMEOUT_CMD=
runner_init_timeout_cmd() {
    if [ -n "$NGINX_RUNNER_TIMEOUT_CMD" ]; then
        return 0
    fi
    if command -v timeout >/dev/null 2>&1; then
        NGINX_RUNNER_TIMEOUT_CMD='timeout'
        return 0
    fi
    if busybox timeout 2>&1 | grep -qi 'usage'; then
        NGINX_RUNNER_TIMEOUT_CMD='busybox timeout'
        return 0
    fi
    runner_log "timeout command not available"
    return 1
}

# runner_run_with_timeout <seconds> <cmd...>
# Runs a command under the detected timeout. Returns the command's exit code,
# or 124 (timeout's convention) when the deadline is hit.
runner_run_with_timeout() {
    sec=$1
    shift
    # Word-split is intentional: NGINX_RUNNER_TIMEOUT_CMD may be "busybox timeout".
    # shellcheck disable=SC2086
    $NGINX_RUNNER_TIMEOUT_CMD "$sec" "$@"
}

# runner_port_listening <port> -> 0 if something is LISTENing on the port.
# Best-effort: uses ss, then netstat; if neither exists we cannot tell and
# report "not listening" so the caller falls back to a fixed settle delay.
runner_port_listening() {
    port=$1
    if command -v ss >/dev/null 2>&1; then
        ss -ltn 2>/dev/null | grep -q ":${port}[[:space:]]" && return 0
        return 1
    fi
    if command -v netstat >/dev/null 2>&1; then
        netstat -ltn 2>/dev/null | grep -q ":${port}[[:space:]]" && return 0
        return 1
    fi
    return 1
}

# runner_wait_ports_free [max_seconds]
# Waits until none of NGINX_RUNNER_PORTS is LISTENing, up to max_seconds.
# Prevents a residual nginx from one phase from making the next phase's bind
# fail (a false negative). Best-effort when no socket tool is available.
runner_wait_ports_free() {
    max=${1:-10}
    i=0
    while [ "$i" -lt "$max" ]; do
        busy=0
        for port in $NGINX_RUNNER_PORTS; do
            if runner_port_listening "$port"; then
                busy=1
                break
            fi
        done
        [ "$busy" -eq 0 ] && return 0
        sleep 1
        i=$((i + 1))
    done
    runner_log "ports still busy after ${max}s: $NGINX_RUNNER_PORTS"
    return 1
}

# runner_ensure_packages
# Installs the full package superset required by any phase/smoke script.
# Uses a sentinel file so apk add runs at most once per QEMU session, whether
# called from `all` (first stage installs, rest skip) or a standalone phase run.
NGINX_PKGS_SENTINEL="/tmp/nginx-pkgs-installed"
NGINX_PKGS="nginx curl busybox-extras procps netcat-openbsd"
runner_ensure_packages() {
    [ -f "$NGINX_PKGS_SENTINEL" ] && return 0
    . /usr/bin/nginx-alpine-mirror.sh
    nginx_apk_add_with_fallback $NGINX_PKGS || return 1
    touch "$NGINX_PKGS_SENTINEL"
}

# runner_isolate_after_phase
# Unconditional per-phase cleanup, run after every phase regardless of result.
# This is the second line of defence behind each phase's own EXIT/TERM trap.
runner_isolate_after_phase() {
    # 1. Reap any nginx the phase left behind (graceful, then forced).
    killall -q nginx 2>/dev/null || true
    sleep 1
    killall -q -9 nginx 2>/dev/null || true
    # 2. Confirm the shared listen ports are released before the next phase.
    runner_wait_ports_free 10 || true
    # 3. Drop per-phase temp trees so disk state does not accumulate.
    rm -rf /tmp/nginx-phase* /tmp/nginx-tests 2>/dev/null || true
}
