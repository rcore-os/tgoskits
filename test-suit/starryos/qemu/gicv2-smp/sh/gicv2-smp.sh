#!/bin/sh

fail_marker=STARRY_GICV2_SMP_FAILED
pids=

if ! command -v taskset >/dev/null 2>&1; then
    echo "$fail_marker: taskset is unavailable"
    exit 1
fi

# Pin one sleeping worker to every CPU. Timer expiry and parent wait wakeups
# exercise the GICv2 SGI path after all secondary CPU interfaces are online.
for cpu in 0 1 2 3; do
    taskset -c "$cpu" sh -c 'sleep 1' &
    pids="$pids $!"
done

for pid in $pids; do
    if ! wait "$pid"; then
        echo "$fail_marker: pinned worker $pid failed"
        exit 1
    fi
done

echo STARRY_GICV2_SMP_PASSED
