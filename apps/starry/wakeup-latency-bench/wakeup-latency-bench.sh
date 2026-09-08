#!/bin/sh
set -u

echo "WAKEUP_LATENCY_APP_START"
/usr/bin/wakeup-latency-bench
status=$?

echo "WAKEUP_LATENCY_APP_DONE rc=$status"
if [ -r /sys/kernel/debug/scheduler_metrics ]; then
    echo "WAKEUP_LATENCY_SCHEDULER_METRICS_START"
    cat /sys/kernel/debug/scheduler_metrics
    echo "WAKEUP_LATENCY_SCHEDULER_METRICS_DONE"
fi
if [ "$status" -eq 0 ]; then
    echo "WAKEUP_LATENCY_APP_PASSED"
else
    echo "WAKEUP_LATENCY_APP_FAILED"
fi
exit "$status"
