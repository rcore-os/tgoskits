#!/bin/sh
set -u

echo "FUTEX_PING_PONG_APP_START"
/usr/bin/futex-ping-pong-bench
status=$?

echo "FUTEX_PING_PONG_APP_DONE rc=$status"
if [ "$status" -eq 0 ]; then
    echo "FUTEX_PING_PONG_APP_PASSED"
else
    echo "FUTEX_PING_PONG_APP_FAILED"
fi
exit "$status"
