#!/bin/sh
# apache-runner.sh - unified entry point for the Starry Apache test suite.
#
# The control flow mirrors `apps/starry/nginx/runner/nginx-runner.sh`:
#   - one timeout probe in `main`
#   - per-phase begin/pass/fail markers
#   - fail-fast `all`
#   - runner-owned post-stage isolation

. "$(CDPATH= cd -- "$(dirname "$0")" && pwd)/apache-runner-lib.sh"

APACHE_RUNNER_STAGE_TIMEOUT="${APACHE_RUNNER_STAGE_TIMEOUT:-1200}"

APACHE_RUNNER_ALL_STAGES='
smoke
phase20
phase30
phase40
phase50
phase55
phase70
phase80
'

runner_pass() {
    printf 'APACHE_RUNNER_PASSED mode=%s\n' "$1"
    exit 0
}

runner_fail() {
    printf 'APACHE_RUNNER_FAILED phase=%s rc=%s\n' "$1" "$2"
    exit 1
}

runner_run_stage() {
    id=$1
    script=$2
    if [ ! -x "$script" ] && [ ! -f "$script" ]; then
        printf 'APACHE_RUNNER_PHASE_FAIL phase=%s rc=127\n' "$id"
        apache_runner_log "stage script missing: $script"
        return 127
    fi
    printf 'APACHE_RUNNER_PHASE_BEGIN phase=%s\n' "$id"
    apache_runner_log "BEGIN $id"
    apache_runner_run_with_timeout "$APACHE_RUNNER_STAGE_TIMEOUT" sh "$script"
    rc=$?
    apache_runner_isolate_after_stage
    if [ "$rc" -eq 0 ]; then
        printf 'APACHE_RUNNER_PHASE_PASS phase=%s\n' "$id"
    else
        printf 'APACHE_RUNNER_PHASE_FAIL phase=%s rc=%s\n' "$id" "$rc"
    fi
    return "$rc"
}

mode_smoke() {
    script=$(apache_runner_resolve_script smoke) || runner_fail smoke 2
    runner_run_stage smoke "$script" || runner_fail smoke "$?"
    runner_pass smoke
}

mode_phase() {
    id=$1
    [ -n "$id" ] || { apache_runner_log "phase mode requires a phase id"; runner_fail phase 2; }
    script=$(apache_runner_resolve_script "$id") || { apache_runner_log "unknown phase id: $id"; runner_fail "$id" 2; }
    runner_run_stage "$id" "$script" || runner_fail "$id" "$?"
    runner_pass "phase:$id"
}

mode_all() {
    while read -r id; do
        [ -n "$id" ] || continue
        script=$(apache_runner_resolve_script "$id") || { runner_fail "$id" 2; }
        runner_run_stage "$id" "$script" || runner_fail "$id" "$?"
    done <<EOF_STAGES
$APACHE_RUNNER_ALL_STAGES
EOF_STAGES
    runner_pass all
}

mode_stress() {
    apache_runner_log "stress suite not implemented yet; skipping"
    runner_pass stress
}

mode_debug() {
    name=$1
    [ -n "$name" ] || { apache_runner_log "debug mode requires a name"; runner_fail debug 2; }
    script=$(apache_runner_debug_script "$name") || { apache_runner_log "unknown debug name: $name"; runner_fail "debug:$name" 2; }
    runner_run_stage "debug:$name" "$script" || runner_fail "debug:$name" "$?"
    runner_pass "debug:$name"
}

main() {
    APACHE_APP_DIR=${APACHE_APP_DIR:-$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)}
    export APACHE_APP_DIR
    apache_runner_init_timeout_cmd || runner_fail init 1
    mode=${1:-}
    [ "$#" -gt 0 ] && shift
    case "$mode" in
        smoke)  mode_smoke ;;
        phase)  mode_phase "${1:-}" ;;
        all)    mode_all ;;
        stress) mode_stress ;;
        debug)  mode_debug "${1:-}" ;;
        *)
            apache_runner_log "usage: $0 {smoke|phase <id>|all|stress|debug <name>}"
            runner_fail usage 2
            ;;
    esac
}

main "$@"
