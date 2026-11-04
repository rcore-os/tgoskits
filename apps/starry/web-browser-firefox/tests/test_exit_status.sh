#!/bin/sh
set -eu

here=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
. "$here/../firefox_exit_status.sh"

fail_test() {
    echo "FIREFOX_EXIT_STATUS_TEST_FAILED: $*" >&2
    exit 1
}

sh -c "exit 0" &
wait_child_status $!
[ "$child_wait_status" -eq 0 ] \
    || fail_test "normal child exit reported status $child_wait_status"
if ! output=$(check_firefox_exit_status "$child_wait_status" 2>&1); then
    fail_test "normal exit was classified as failure"
fi
case "$output" in
    *WEB_BROWSER_DIAG*) ;;
    *) fail_test "normal exit did not produce the diagnostic" ;;
esac

sh -c "exit 1" &
wait_child_status $!
[ "$child_wait_status" -eq 1 ] \
    || fail_test "exit 1 child reported status $child_wait_status"
if check_firefox_exit_status "$child_wait_status" >/dev/null 2>&1; then
    fail_test "exit 1 was classified as a normal exit"
fi

sh -c "kill -KILL \$\$" &
wait_child_status $!
[ "$child_wait_status" -eq 137 ] \
    || fail_test "SIGKILL child reported status $child_wait_status"
if check_firefox_exit_status "$child_wait_status" >/dev/null 2>&1; then
    fail_test "SIGKILL exit was classified as a normal exit"
fi

echo "FIREFOX_EXIT_STATUS_TEST_PASSED"
