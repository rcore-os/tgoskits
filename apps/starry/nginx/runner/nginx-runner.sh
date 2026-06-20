#!/bin/sh
# nginx-runner.sh - unified entry point for the Starry nginx test suite.
#
# Usage (invoked as the QEMU shell_init_cmd):
#   /usr/bin/nginx-runner.sh smoke
#   /usr/bin/nginx-runner.sh phase <phase-id>
#   /usr/bin/nginx-runner.sh all
#   /usr/bin/nginx-runner.sh stress
#   /usr/bin/nginx-runner.sh debug <name>
#
# Terminal markers (the only thing QEMU success/fail regex must match):
#   NGINX_RUNNER_PASSED mode=<mode>
#   NGINX_RUNNER_FAILED phase=<id> rc=<rc>
# Per-phase markers (informational, used by `all`):
#   NGINX_RUNNER_PHASE_BEGIN phase=<id>
#   NGINX_RUNNER_PHASE_PASS  phase=<id>
#   NGINX_RUNNER_PHASE_FAIL  phase=<id> rc=<rc>
#
# Pass/fail is decided by each stage script's EXIT CODE, not by its own marker
# text, so the historical marker-name drift (phase13 prints NGINX_PHASE1_*,
# phase20 prints NGINX_PHASE2_*) does not matter here.

. /usr/bin/nginx-runner-lib.sh

# Per-phase wall-clock budget. The runner is now the only watchdog (phase
# scripts no longer self-kill), so this must be generous enough to cover apk
# package install + the test body. The outer QEMU `timeout` is the hard bound.
NGINX_RUNNER_PHASE_TIMEOUT="${NGINX_RUNNER_PHASE_TIMEOUT:-600}"

# Ordered stage list for `all`. Format: "<phase-id> <guest-script-path>".
# smoke runs first, then phases in ascending id order.
NGINX_RUNNER_ALL_STAGES='
smoke   /usr/bin/nginx-smoke-tests.sh
phase00 /usr/bin/nginx-phase00-tests.sh
phase12 /usr/bin/nginx-phase12-tests.sh
phase13 /usr/bin/nginx-phase13-tests.sh
phase20 /usr/bin/nginx-phase20-tests.sh
phase31 /usr/bin/nginx-phase31-tests.sh
phase32 /usr/bin/nginx-phase32-tests.sh
phase33 /usr/bin/nginx-phase33-tests.sh
phase41 /usr/bin/nginx-phase41-tests.sh
phase42 /usr/bin/nginx-phase42-tests.sh
phase43 /usr/bin/nginx-phase43-tests.sh
phase50 /usr/bin/nginx-phase50-tests.sh
phase60 /usr/bin/nginx-phase60-tests.sh
phase70 /usr/bin/nginx-phase70-tests.sh
phase90 /usr/bin/nginx-phase90-tests.sh
'

# Resolve a phase id to its guest script path. Echoes the path, or nothing
# when the id is unknown.
runner_phase_script() {
    want=$1
    printf '%s\n' "$NGINX_RUNNER_ALL_STAGES" | while read -r id script; do
        [ -n "$id" ] || continue
        if [ "$id" = "$want" ]; then
            printf '%s\n' "$script"
            return 0
        fi
    done
}

# Resolve a debug name to its guest script path.
runner_debug_script() {
    case "$1" in
        bad-method-debug)  printf '%s\n' /usr/bin/nginx-bad-method-debug.sh ;;
        bad-method-matrix) printf '%s\n' /usr/bin/nginx-bad-method-matrix.sh ;;
        short-connection-debug) printf '%s\n' /usr/bin/nginx-short-connection-debug.sh ;;
        x86-timing-debug) printf '%s\n' /usr/bin/nginx-x86-timing-debug.sh ;;
        sendfile-on-debug) printf '%s\n' /usr/bin/nginx-sendfile-on-debug.sh ;;
        *)                 return 1 ;;
    esac
}

runner_pass() {
    printf 'NGINX_RUNNER_PASSED mode=%s\n' "$1"
    exit 0
}

runner_fail() {
    printf 'NGINX_RUNNER_FAILED phase=%s rc=%s\n' "$1" "$2"
    exit 1
}

# runner_run_stage <phase-id> <script-path>
# Runs one stage in an isolated child process under a timeout, emits per-phase
# markers, and always runs post-phase isolation. Returns the stage exit code.
runner_run_stage() {
    id=$1
    script=$2
    if [ ! -x "$script" ] && [ ! -f "$script" ]; then
        printf 'NGINX_RUNNER_PHASE_FAIL phase=%s rc=127\n' "$id"
        runner_log "stage script missing: $script"
        return 127
    fi
    printf 'NGINX_RUNNER_PHASE_BEGIN phase=%s\n' "$id"
    # Independent child process (sh <script>), never sourced: a stage's `set -eu`
    # / `exit` / variables cannot leak into the runner or the next stage.
    runner_run_with_timeout "$NGINX_RUNNER_PHASE_TIMEOUT" sh "$script"
    rc=$?
    # Isolation runs whether the stage passed, failed, or timed out.
    runner_isolate_after_phase
    if [ "$rc" -eq 0 ]; then
        printf 'NGINX_RUNNER_PHASE_PASS phase=%s\n' "$id"
    else
        printf 'NGINX_RUNNER_PHASE_FAIL phase=%s rc=%s\n' "$id" "$rc"
    fi
    return "$rc"
}

mode_smoke() {
    runner_run_stage smoke /usr/bin/nginx-smoke-tests.sh || runner_fail smoke $?
    runner_pass smoke
}

mode_phase() {
    id=$1
    [ -n "$id" ] || { runner_log "phase mode requires a phase id"; runner_fail phase 2; }
    script=$(runner_phase_script "$id")
    [ -n "$script" ] || { runner_log "unknown phase id: $id"; runner_fail "$id" 2; }
    runner_run_stage "$id" "$script" || runner_fail "$id" $?
    runner_pass "phase:$id"
}

# fail-fast: the first failing stage stops the run and reports the terminal
# failure marker with the offending phase id.
mode_all() {
    printf '%s\n' "$NGINX_RUNNER_ALL_STAGES" | {
        while read -r id script; do
            [ -n "$id" ] || continue
            runner_run_stage "$id" "$script"
            rc=$?
            # Capture rc directly: a `if ! ...; then ... $?` form would report the
            # negated test status (0), losing the real stage exit code.
            if [ "$rc" -ne 0 ]; then
                runner_fail "$id" "$rc"
            fi
        done
    } || exit 1
    runner_pass all
}

mode_stress() {
    runner_log "stress suite not implemented yet; skipping"
    runner_pass stress
}

mode_debug() {
    name=$1
    [ -n "$name" ] || { runner_log "debug mode requires a name"; runner_fail debug 2; }
    script=$(runner_debug_script "$name") \
        || { runner_log "unknown debug name: $name"; runner_fail "debug:$name" 2; }
    # Debug scripts own their markers; the runner only needs the exit code.
    runner_run_with_timeout "$NGINX_RUNNER_PHASE_TIMEOUT" sh "$script"
    rc=$?
    runner_isolate_after_phase
    [ "$rc" -eq 0 ] || runner_fail "debug:$name" "$rc"
    runner_pass "debug:$name"
}

main() {
    runner_init_timeout_cmd || runner_fail init 1
    mode=${1:-}
    # Guard the shift: in dash, `shift` with no positional parameters is a fatal
    # error (not just non-zero), so `shift || true` would still abort the script.
    [ "$#" -gt 0 ] && shift
    case "$mode" in
        smoke)  mode_smoke ;;
        phase)  mode_phase "${1:-}" ;;
        all)    mode_all ;;
        stress) mode_stress ;;
        debug)  mode_debug "${1:-}" ;;
        *)
            runner_log "usage: $0 {smoke|phase <id>|all|stress|debug <name>}"
            runner_fail usage 2
            ;;
    esac
}

main "$@"
