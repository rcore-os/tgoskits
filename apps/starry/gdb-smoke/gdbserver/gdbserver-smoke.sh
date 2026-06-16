#!/bin/sh
set -eu

log=/tmp/gdbserver-smoke.log
gdb_timeout_seconds=${GDBSERVER_SMOKE_TIMEOUT:-90}

run_gdb_batch() {
    script=$1
    target=$2

    set -- /usr/bin/gdb -q -batch
    if [ "${GDBSERVER_SMOKE_GDB_DEBUG:-0}" = 1 ]; then
        set -- "$@" -ex "set debug remote 1" -ex "set debug infrun 1"
    fi
    set -- "$@" -x "$script" "$target"

    if command -v timeout >/dev/null 2>&1; then
        timeout -s KILL "$gdb_timeout_seconds" "$@"
    else
        "$@"
    fi
}

run_remote_smoke() {
    target=$1
    script=$2

    echo "GDBSERVER_PHASE_START target=$target script=$script"
    rm -f "$log"
    set -- /usr/bin/gdbserver
    if [ "${GDBSERVER_SMOKE_SERVER_DEBUG:-0}" = 1 ]; then
        set -- "$@" --debug
    fi
    set -- "$@" 0.0.0.0:1234 "$target"
    "$@" >"$log" 2>&1 &
    server_pid=$!
    trap 'kill "$server_pid" 2>/dev/null || true' EXIT

    listening=0
    for _ in 1 2 3 4 5 6 7 8 9 10; do
        if grep -q "Listening on port 1234" "$log"; then
            listening=1
            break
        fi
        sleep 1
    done

    cat "$log"
    if [ "$listening" -ne 1 ]; then
        echo "FAIL: gdbserver did not start listening for $target"
        exit 1
    fi

    if ! run_gdb_batch "$script" "$target"; then
        echo "FAIL: gdb batch failed for $target with $script"
        cat "$log"
        exit 1
    fi

    if ! wait "$server_pid"; then
        cat "$log"
        exit 1
    fi
    trap - EXIT

    cat "$log"
    echo "GDBSERVER_PHASE_DONE target=$target"
}

run_remote_smoke /usr/bin/gdbserver-smoke-target /usr/bin/gdbserver-smoke.gdb
if [ "${GDBSERVER_SMOKE_THREADS:-0}" = 1 ]; then
    run_remote_smoke /usr/bin/gdb-native-thread-target /usr/bin/gdbserver-threads.gdb
else
    echo "GDBSERVER_THREADS_SKIPPED"
fi

echo GDBSERVER_SMOKE_DONE
