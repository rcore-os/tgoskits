#!/bin/sh
# Wait and failure propagation shared by the Firefox smoke test.

wait_child_status() {
    child_wait_status=0
    wait "$1" 2>/dev/null || child_wait_status=$?
}

firefox_terminate() {
    kill -TERM "$1" 2>/dev/null
}

finalize_firefox_process() {
    firefox_pid="$1"
    # Only a TERM sent after a successful liveness precheck may explain 143.
    firefox_kill_status=1
    if kill -0 "$firefox_pid" 2>/dev/null; then
        firefox_kill_status=0
        firefox_terminate "$firefox_pid" || firefox_kill_status=$?
    fi
    wait_child_status "$firefox_pid"

    if [ "$firefox_kill_status" -eq 0 ] && [ "$child_wait_status" -eq 143 ]; then
        return 0
    fi

    check_firefox_exit_status "$child_wait_status"
}

check_firefox_exit_status() {
    status="$1"
    if [ "$status" -eq 0 ]; then
        echo "WEB_BROWSER_DIAG firefox exited normally after the remote page was fetched; continuing with the bundled render probe"
        return 0
    fi

    echo "WEB_BROWSER_DIAG firefox exited abnormally after the remote page was fetched: status=$status" >&2
    return 1
}
