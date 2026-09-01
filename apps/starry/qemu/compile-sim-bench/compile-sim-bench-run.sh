#!/bin/sh

set -u

program=${COMPILE_SIM_BIN:-/usr/bin/compile-sim-bench}
if "$program" "$@"; then
    exit 0
else
    status=$?
    printf 'COMPILE_SIM_BENCH_FAILED status=%s\n' "$status" >&2
    exit "$status"
fi
