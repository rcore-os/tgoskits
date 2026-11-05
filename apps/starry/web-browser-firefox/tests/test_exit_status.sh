#!/bin/sh
set -eu

here=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
. "$here/../firefox_exit_status.sh"

test_tmp=
child_pid=
case_serial=0
test_failed=0

fail_test() {
    echo "FIREFOX_EXIT_STATUS_TEST_FAILED: $*" >&2
    exit 1
}

fail_assert() {
    echo "FIREFOX_EXIT_STATUS_TEST_FAILED: $*" >&2
    test_failed=1
}

cleanup() {
    if [ -n "$child_pid" ]; then
        kill -KILL "$child_pid" 2>/dev/null || true
        wait "$child_pid" 2>/dev/null || true
        child_pid=
    fi
    if [ -n "$test_tmp" ]; then
        rm -rf "$test_tmp"
    fi
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM

test_tmp=$(mktemp -d "${TMPDIR:-/tmp}/firefox-exit-status.XXXXXX")

# Start a child that blocks until the lifecycle operation signals it. The
# optional first argument makes TERM exit with that status. The `sigkill` mode
# instead turns a control-pipe write into a real SIGKILL, reproducing the race
# between the monitor's final `kill -0` and the lifecycle operation's kill.
start_waiting_child() {
    term_status=${1:-}
    after_control=${2:-wait}
    case_serial=$((case_serial + 1))
    ready_fifo="$test_tmp/ready.$case_serial"
    control_fifo="$test_tmp/control.$case_serial"
    mkfifo "$ready_fifo" "$control_fifo"

    if [ "$after_control" = sigkill ]; then
        sh -c '
            printf "ready\n" > "$1"
            exec 3<>"$2"
            IFS= read -r _ <&3
            kill -KILL $$
        ' sh "$ready_fifo" "$control_fifo" &
    elif [ -n "$term_status" ]; then
        sh -c '
            trap "exit $3" TERM
            printf "ready\n" > "$1"
            exec 3<>"$2"
            IFS= read -r _ <&3
        ' sh "$ready_fifo" "$control_fifo" "$term_status" &
    else
        sh -c '
            printf "ready\n" > "$1"
            exec 3<>"$2"
            IFS= read -r _ <&3
        ' sh "$ready_fifo" "$control_fifo" &
    fi

    child_pid=$!
    IFS= read -r ready < "$ready_fifo" \
        || fail_test "child did not become ready"
    [ "$ready" = ready ] \
        || fail_test "child readiness event was '$ready'"
    kill -0 "$child_pid" 2>/dev/null \
        || fail_test "child was not alive after its readiness event"
}

finalize_current_child() {
    lifecycle_status=0
    finalize_firefox_process "$child_pid" || lifecycle_status=$?
    child_pid=
    return "$lifecycle_status"
}

sh -c "exit 0" &
direct_pid=$!
wait_child_status "$direct_pid"
[ "$child_wait_status" -eq 0 ] \
    || fail_assert "normal child exit reported status $child_wait_status"
if ! output=$(check_firefox_exit_status "$child_wait_status" 2>&1); then
    fail_assert "normal exit was classified as failure"
fi
case "$output" in
    *WEB_BROWSER_DIAG*) ;;
    *) fail_assert "normal exit did not produce the diagnostic" ;;
esac

sh -c "exit 1" &
direct_pid=$!
wait_child_status "$direct_pid"
[ "$child_wait_status" -eq 1 ] \
    || fail_assert "exit 1 child reported status $child_wait_status"
if check_firefox_exit_status "$child_wait_status" >/dev/null 2>&1; then
    fail_assert "exit 1 was classified as a normal exit"
fi

sh -c "kill -KILL \$\$" &
direct_pid=$!
wait_child_status "$direct_pid"
[ "$child_wait_status" -eq 137 ] \
    || fail_assert "SIGKILL child reported status $child_wait_status"
if check_firefox_exit_status "$child_wait_status" >/dev/null 2>&1; then
    fail_assert "SIGKILL exit was classified as a normal exit"
fi

start_waiting_child 0
if finalize_current_child; then
    lifecycle_status=0
else
    lifecycle_status=$?
fi
[ "$lifecycle_status" -eq 0 ] \
    || fail_assert "normal lifecycle exit was rejected"
[ "$child_wait_status" -eq 0 ] \
    || fail_assert "normal lifecycle wait reported status $child_wait_status"

start_waiting_child
if finalize_current_child; then
    lifecycle_status=0
else
    lifecycle_status=$?
fi
[ "$lifecycle_status" -eq 0 ] \
    || fail_assert "terminated lifecycle exit 143 was rejected"
[ "$child_wait_status" -eq 143 ] \
    || fail_assert "actively terminated child reported status $child_wait_status"

start_waiting_child 1
if finalize_current_child; then
    lifecycle_status=0
else
    lifecycle_status=$?
fi
[ "$child_wait_status" -eq 1 ] \
    || fail_assert "abnormal lifecycle wait reported status $child_wait_status"
[ "$lifecycle_status" -ne 0 ] \
    || fail_assert "abnormal lifecycle exit was accepted"

# A failed kill means that 143 cannot be attributed to the lifecycle
# operation. The child still exits from TERM, and the real wait must reject it.
start_waiting_child
firefox_terminate() {
    kill "$1" 2>/dev/null
    return 1
}
if finalize_current_child; then
    lifecycle_status=0
else
    lifecycle_status=$?
fi
[ "$child_wait_status" -eq 143 ] \
    || fail_assert "TERM after failed kill reported status $child_wait_status"
[ "$lifecycle_status" -ne 0 ] \
    || fail_assert "status 143 after a failed kill was accepted"

# Cover children that have already exited but whose status has not been
# consumed by wait. The external test timeout bounds the reaping observation.
for exited_status in 0 143; do
    case "$exited_status" in
        0) sh -c "exit 0" & ;;
        143) sh -c "kill -TERM \$\$" & ;;
    esac
    child_pid=$!
    while kill -0 "$child_pid" 2>/dev/null; do :; done

    terminate_called=0
    firefox_terminate() {
        terminate_called=1
        return 0
    }
    if finalize_current_child; then
        lifecycle_status=0
    else
        lifecycle_status=$?
    fi
    [ "$terminate_called" -eq 0 ] \
        || fail_assert "already-exited status $exited_status received TERM"
    [ "$child_wait_status" -eq "$exited_status" ] \
        || fail_assert "already-exited child reported status $child_wait_status"
    if [ "$exited_status" -eq 0 ]; then
        [ "$lifecycle_status" -eq 0 ] \
            || fail_assert "already-exited normal status 0 was rejected"
    else
        [ "$lifecycle_status" -ne 0 ] \
            || fail_assert "already-exited status 143 was accepted"
    fi
done

# The monitor's last real check succeeded. The lifecycle TERM then loses a race
# to a real child SIGKILL. Both a successful and failed kill delivery must
# reject the real wait status, and the rendering-probe canary must stay untouched.
run_sigkill_race_case() {
    terminate_status=$1
    start_waiting_child "" sigkill
    kill -0 "$child_pid" 2>/dev/null \
        || fail_test "SIGKILL race child died before the lifecycle operation"
    firefox_terminate() {
        printf 'terminate\n' > "$control_fifo"
        return "$terminate_status"
    }
    probe_canary=0
    if finalize_current_child; then
        probe_canary=1
    fi
    [ "$firefox_kill_status" -eq "$terminate_status" ] \
        || fail_assert "SIGKILL case did not exercise terminate status $terminate_status"
    [ "$child_wait_status" -eq 137 ] \
        || fail_assert "final lifecycle wait reported SIGKILL status $child_wait_status"
    [ "$lifecycle_status" -ne 0 ] \
        || fail_assert "SIGKILL after terminate status $terminate_status was accepted"
    [ "$probe_canary" -eq 0 ] \
        || fail_assert "SIGKILL after terminate status $terminate_status ran the render probe canary"
}

run_sigkill_race_case 0
run_sigkill_race_case 1

[ "$test_failed" -eq 0 ] || exit 1
echo "FIREFOX_EXIT_STATUS_TEST_PASSED"
