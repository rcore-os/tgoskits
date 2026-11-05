#!/bin/bash
# Own the logger and progress process until all output has been flushed.
selfbuild_log_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
progress_pid=""
logger_pid=""

compile_unit_count() {
    awk '/^[[:space:]]*Compiling / { count++ } END { print count+0 }' "$run_log"
}

emit_progress() {
    python3 "$selfbuild_log_dir/selfbuild_progress.py" \
        once "$run_log" "$marker" "$1" "$2" "$3"
}

start_progress_sampler() {
    python3 "$selfbuild_log_dir/selfbuild_progress.py" \
        watch "$run_log" "$marker" "$1" "$2" "$3" &
    progress_pid=$!
}

stop_progress_sampler() {
    if [ -n "$progress_pid" ]; then
        if ! kill "$progress_pid"; then
            printf 'progress sampler already exited: %s\n' "$progress_pid" >&2
        fi
        local status=0
        wait "$progress_pid" || status=$?
        progress_pid=""
        case "$status" in
            0|143) ;;
            *) printf '===%s-FAIL reason=progress-sampler rc=%s===\n' "$marker" "$status"; return 1 ;;
        esac
    fi
}

finish_logging() {
    local status=$?
    trap - EXIT
    if ! stop_progress_sampler; then
        status=1
    fi
    exec 1>&3 2>&4
    exec 3>&- 4>&-
    local logger_status=0
    wait "$logger_pid" || logger_status=$?
    if [ "$logger_status" != 0 ]; then
        printf '===%s-FAIL reason=log-copy rc=%s===\n' "$marker" "$logger_status"
        status=1
    fi
    exit "$status"
}


begin_build_log() {
    run_log=$1
    marker=$2
    exec 3>&1 4>&2
    exec > >(tee "$run_log") 2>&1
    logger_pid=$!
    trap finish_logging EXIT
}
