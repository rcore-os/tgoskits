#!/bin/sh

set -eu

trap 'poweroff -f' EXIT INT TERM
printf 'LINUX_RT_COMPILE_SIM_BOOTED\n'
if /usr/bin/compile-sim-bench --benchmark; then
    printf 'LINUX_RT_COMPILE_SIM_PASSED\n'
else
    status=$?
    printf 'LINUX_RT_COMPILE_SIM_FAILED status=%s\n' "$status" >&2
    exit "$status"
fi
