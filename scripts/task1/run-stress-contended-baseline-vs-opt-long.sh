#!/usr/bin/env bash
set -euo pipefail
export BASELINE_CASE="mixed-rt-stress-baseline-contended-long"
export BASELINE_DESC="pCPU3 contended + emulated timer + slow-vtimer 1ms, no sched-cfs"
exec "$(dirname "$0")/run-stress-baseline-vs-opt-long.sh" "$@"
