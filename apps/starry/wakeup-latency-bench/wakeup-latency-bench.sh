#!/bin/sh
set -u

echo "WAKEUP_LATENCY_APP_START"
/usr/bin/wakeup-latency-bench
status=$?

echo "WAKEUP_LATENCY_APP_DONE rc=$status"
if [ "$status" -eq 0 ]; then
    echo "WAKEUP_LATENCY_APP_PASSED"
else
    echo "WAKEUP_LATENCY_APP_FAILED"
fi
exit "$status"
