#!/bin/sh

APACHE_RUNNER_TIMEOUT_CMD=
APACHE_RUNNER_SLEEP_CMD=
APACHE_RUNNER_PORTS="8080"
APACHE_RUNNER_PKGS_SENTINEL="/tmp/apache-pkgs-installed"
APACHE_RUNNER_PKGS="apache2 apache2-utils curl busybox-extras procps netcat-openbsd coreutils"

apache_runner_log() { printf 'APACHE_RUNNER_LOG: %s\n' "$*"; }

apache_runner_init_timeout_cmd() {
    if [ -n "$APACHE_RUNNER_TIMEOUT_CMD" ]; then
        return 0
    fi
    if busybox timeout 2>&1 | grep -qi 'usage'; then
        APACHE_RUNNER_TIMEOUT_CMD='busybox timeout'
        return 0
    fi
    if command -v timeout >/dev/null 2>&1; then
        APACHE_RUNNER_TIMEOUT_CMD='timeout'
        return 0
    fi
    apache_runner_log "timeout command not available"
    return 1
}

apache_runner_run_with_timeout() {
    sec=$1
    shift
    # shellcheck disable=SC2086
    $APACHE_RUNNER_TIMEOUT_CMD "$sec" "$@"
}

apache_runner_init_sleep_cmd() {
    if [ -n "$APACHE_RUNNER_SLEEP_CMD" ]; then
        return 0
    fi
    if busybox sleep 0 >/dev/null 2>&1; then
        APACHE_RUNNER_SLEEP_CMD='busybox sleep'
        return 0
    fi
    if sleep 0 >/dev/null 2>&1; then
        APACHE_RUNNER_SLEEP_CMD='sleep'
        return 0
    fi
    apache_runner_log "sleep command not available"
    return 1
}

apache_runner_sleep() {
    sec=$1
    apache_runner_init_sleep_cmd || return 1
    # shellcheck disable=SC2086
    $APACHE_RUNNER_SLEEP_CMD "$sec"
}

apache_runner_resolve_script() {
    name=$1
    app_dir=${APACHE_APP_DIR:-}
    case "$name" in
        smoke)
            if [ -n "$app_dir" ] && [ -f "$app_dir/smoke/apache-smoke-tests.sh" ]; then
                printf '%s\n' "$app_dir/smoke/apache-smoke-tests.sh"
            else
                printf '/usr/bin/apache-smoke-tests.sh\n'
            fi
            ;;
        phase20)
            if [ -n "$app_dir" ] && [ -f "$app_dir/phase/apache-2-0-mpm-prefork-tests.sh" ]; then
                printf '%s\n' "$app_dir/phase/apache-2-0-mpm-prefork-tests.sh"
            else
                printf '/usr/bin/apache-phase20-tests.sh\n'
            fi
            ;;
        phase30)
            if [ -n "$app_dir" ] && [ -f "$app_dir/phase/apache-3-0-http-static-tests.sh" ]; then
                printf '%s\n' "$app_dir/phase/apache-3-0-http-static-tests.sh"
            else
                printf '/usr/bin/apache-phase30-tests.sh\n'
            fi
            ;;
        phase40)
            if [ -n "$app_dir" ] && [ -f "$app_dir/phase/apache-4-0-directory-access-tests.sh" ]; then
                printf '%s\n' "$app_dir/phase/apache-4-0-directory-access-tests.sh"
            else
                printf '/usr/bin/apache-phase40-tests.sh\n'
            fi
            ;;
        phase50)
            if [ -n "$app_dir" ] && [ -f "$app_dir/phase/apache-5-0-log-lifecycle-tests.sh" ]; then
                printf '%s\n' "$app_dir/phase/apache-5-0-log-lifecycle-tests.sh"
            else
                printf '/usr/bin/apache-phase50-tests.sh\n'
            fi
            ;;
        phase55)
            if [ -n "$app_dir" ] && [ -f "$app_dir/phase/apache-5-5-sendfile-range-tests.sh" ]; then
                printf '%s\n' "$app_dir/phase/apache-5-5-sendfile-range-tests.sh"
            else
                printf '/usr/bin/apache-phase55-tests.sh\n'
            fi
            ;;
        phase70)
            if [ -n "$app_dir" ] && [ -f "$app_dir/phase/apache-7-0-cgi-tests.sh" ]; then
                printf '%s\n' "$app_dir/phase/apache-7-0-cgi-tests.sh"
            else
                printf '/usr/bin/apache-phase70-tests.sh\n'
            fi
            ;;
        phase80)
            if [ -n "$app_dir" ] && [ -f "$app_dir/phase/apache-8-0-module-feature-tests.sh" ]; then
                printf '%s\n' "$app_dir/phase/apache-8-0-module-feature-tests.sh"
            else
                printf '/usr/bin/apache-phase80-tests.sh\n'
            fi
            ;;
        *) return 1 ;;
    esac
}

apache_runner_debug_script() {
    name=$1
    app_dir=${APACHE_APP_DIR:-}
    case "$name" in
        mpm-prefork-wait)
            rel=debug/apache-mpm-prefork-wait.sh ;;
        phase20-restart)
            rel=debug/apache-phase20-restart.sh ;;
        mpm-thread-futex)
            rel=debug/apache-mpm-thread-futex.sh ;;
        accept-mutex)
            rel=debug/apache-accept-mutex.sh ;;
        htaccess-pathwalk)
            rel=debug/apache-htaccess-pathwalk.sh ;;
        sendfile-mmap-range)
            rel=debug/apache-sendfile-mmap-range.sh ;;
        graceful-signal)
            rel=debug/apache-graceful-signal.sh ;;
        cgi-pipe-exec)
            rel=debug/apache-cgi-pipe-exec.sh ;;
        log-append-reopen)
            rel=debug/apache-log-append-reopen.sh ;;
        *) return 1 ;;
    esac

    if [ -n "$app_dir" ] && [ -f "$app_dir/$rel" ]; then
        printf '%s\n' "$app_dir/$rel"
        return 0
    fi
    printf '/usr/bin/%s\n' "${rel##*/}"
}

apache_runner_ensure_packages() {
    [ -f "$APACHE_RUNNER_PKGS_SENTINEL" ] && return 0
    if [ -f /usr/bin/apache-alpine-mirror.sh ]; then
        . /usr/bin/apache-alpine-mirror.sh
    elif [ -n "${APACHE_APP_DIR:-}" ] && [ -f "$APACHE_APP_DIR/runner/apache-alpine-mirror.sh" ]; then
        . "$APACHE_APP_DIR/runner/apache-alpine-mirror.sh"
    fi
    apache_apk_add_with_fallback $APACHE_RUNNER_PKGS || return 1
    touch "$APACHE_RUNNER_PKGS_SENTINEL"
}

apache_runner_port_listening() {
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

apache_runner_wait_ports_free() {
    max=${1:-10}
    i=0
    while [ "$i" -lt "$max" ]; do
        busy=0
        for port in $APACHE_RUNNER_PORTS; do
            if apache_runner_port_listening "$port"; then
                busy=1
                break
            fi
        done
        [ "$busy" -eq 0 ] && return 0
        apache_runner_sleep 1
        i=$((i + 1))
    done
    apache_runner_log "ports still busy after ${max}s: $APACHE_RUNNER_PORTS"
    return 1
}

apache_runner_isolate_after_stage() {
    killall -q httpd 2>/dev/null || true
    apache_runner_sleep 1
    killall -q -9 httpd 2>/dev/null || true
    apache_runner_wait_ports_free 10 || true
    rm -rf /tmp/apache-tests /tmp/apache-phase* /tmp/apache-debug* 2>/dev/null || true
}
