#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
duration_sec="${RT_TIMER_AB_DURATION_SEC:-90}"
output_root="${RT_TIMER_AB_OUTPUT_ROOT:-${repo_root}/results/task1/percpu-timer-wheel/formal-ab}"
runner="${repo_root}/scripts/test/rt-partition/run-cyclictest.sh"
global_board="${repo_root}/scripts/test/rt-partition/board-qemu-aarch64-rt-global-timer.toml"
percpu_board="${repo_root}/scripts/test/rt-partition/board-qemu-aarch64-rt.toml"
storm_command="${RT_TIMER_STORM_COMMAND:-rt timer-storm --cpus 0xe --iterations 20000 --expiry-samples 64 --expiry-delay-us 100000}"
zephyr_artifacts="${output_root}/zephyr-no-settle"

OUT_DIR="$zephyr_artifacts" \
    BUILD_DIR="$zephyr_artifacts/build" \
    ZEPHYR_START_GATED=1 \
    ZEPHYR_START_DELAY_MS=0 \
    "${repo_root}/scripts/test/rt-partition/build-zephyr-periodic.sh"

run_case() {
    local name="$1"
    local board="$2"
    RT_OUTPUT_ROOT="${output_root}/${name}" \
        RT_BOARD_CONFIG="$board" \
        RT_SCENARIO=stress-noiso \
        RT_DURATION_SEC="$duration_sec" \
        RT_LOOPS=0 \
        RT_ZEPHYR_IMAGE="${zephyr_artifacts}/zephyr-periodic.bin" \
        RT_ZEPHYR_MANIFEST="${zephyr_artifacts}/zephyr-periodic.manifest" \
        RT_TIMER_STORM_COMMAND="$storm_command" \
        "$runner"
    grep 'RT_TIMER_STORM_' "${output_root}/${name}/stress-noiso/run.log" \
        > "${output_root}/${name}/stress-noiso/timer-storm.txt"
}

run_case global-lock "$global_board"
run_case per-cpu-lock "$percpu_board"

python3 "${repo_root}/scripts/test/rt-partition/summarize-timer-wheel-ab.py" \
    "$output_root"
