#!/usr/bin/env bash
# Task 1 contended + slow-vtimer baseline vs optimized (short stress).
set -euo pipefail
export BASELINE_CASE="mixed-rt-stress-baseline-contended-short"
export BASELINE_DESC="pCPU3 shared w/ Linux vCPU1, emulated timer, slow-vtimer 1ms, no sched-cfs"
exec "$(dirname "$0")/run-stress-strong-baseline-vs-opt-short.sh" "$@"
