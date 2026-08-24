#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
runner="$repo_root/scripts/test/rt-partition/run-cyclictest.sh"
calibration_duration_sec="${RT_CALIBRATION_DURATION_SEC:-120}"
calibration_run_scale="${RT_CALIBRATION_RUN_SCALE:-6}"
out_root="${RT_CALIBRATION_OUTPUT_ROOT:-$repo_root/results/task1/calibration/runs}"
scale_file="${RT_CALIBRATION_FILE:-$repo_root/results/task1/calibration/runtime-scales.env}"

for value in "$calibration_duration_sec" "$calibration_run_scale"; do
    [[ "$value" =~ ^[0-9]+$ ]] && (( value > 0 )) || {
        printf 'error: calibration duration and run scale must be positive integers\n' >&2
        exit 2
    }
done

progress_args=()
for scenario in idle stress-noiso stress-dedicated stress-rt; do
    RT_SCENARIO="$scenario" \
    RT_DURATION_SEC="$calibration_duration_sec" \
    RT_TCG_RUNTIME_SCALE="$calibration_run_scale" \
    RT_OUTPUT_ROOT="$out_root" \
        "$runner"
    progress_args+=("$scenario=$out_root/$scenario/progress.txt")
done

minimum_guest_seconds=$(( calibration_duration_sec * 9 / 10 ))
python3 "$repo_root/scripts/test/rt-partition/calibrate-runtime-scale.py" \
    --output "$scale_file" \
    --minimum-guest-seconds "$minimum_guest_seconds" \
    "${progress_args[@]}"
printf 'calibration written: %s\n' "$scale_file"
