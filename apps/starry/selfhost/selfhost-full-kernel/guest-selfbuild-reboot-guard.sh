#!/bin/sh

# This file is sourced by Alpine's login profile on every Starry boot. The
# app runner writes its current phase to persistent ext4 before doing work, so
# a kernel reset can be distinguished from the initial ready state.

state_file="${SELFHOST_STATE_FILE:-/opt/starry-selfhost.state}"

if [ -r "$state_file" ]; then
    state=""
    run_id=""
    phase="unknown"
    IFS=' ' read -r state run_id phase <"$state_file"

    if [ "$state" = "running" ]; then
        echo "SELF_COMPILE_FAILED: unexpected guest reboot during $phase (run_id=$run_id)"
        sync 2>/dev/null || true

        if [ "${SELFHOST_REBOOT_GUARD_TEST_MODE:-0}" = "1" ]; then
            return 1 2>/dev/null || exit 1
        fi

        poweroff -f 2>/dev/null || poweroff 2>/dev/null || true
        return 1 2>/dev/null || exit 1
    fi
fi

unset state_file state run_id phase
true
