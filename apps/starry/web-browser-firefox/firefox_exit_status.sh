#!/bin/sh
# Wait and failure propagation shared by the Firefox smoke test.

wait_child_status() {
    child_wait_status=0
    wait "$1" 2>/dev/null || child_wait_status=$?
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
