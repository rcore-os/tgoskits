#!/usr/bin/env bash
set -euo pipefail
export BASELINE_CASE="mixed-rt-stress-baseline-host-share-short"
export BASELINE_DESC="pCPU0 host-share + emulated timer, no sched-cfs, 8× CPU stress"
exec "$(dirname "$0")/run-stress-strong-baseline-vs-opt-short.sh" "$@"
