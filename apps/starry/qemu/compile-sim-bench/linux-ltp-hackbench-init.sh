#!/bin/sh

set -eu

trap 'poweroff -f' EXIT INT TERM
printf 'LINUX_RT_LTP_HACKBENCH_BOOTED\n'
if /usr/bin/ltp-hackbench-run benchmark; then
    printf 'LINUX_RT_LTP_HACKBENCH_PASSED\n'
else
    status=$?
    printf 'LINUX_RT_LTP_HACKBENCH_FAILED status=%s\n' "$status" >&2
    exit "$status"
fi
