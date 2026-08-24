#!/usr/bin/env bash
set -euo pipefail

# Run the official-dev shared-core baseline and the dedicated RT partition in
# interleaved order, then summarize the repeated Zephyr tail-latency results.

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
baseline_root="${RT_P1_BASELINE_ROOT:-/home/huhu/tgoskits-rt-baseline}"
output_root="${RT_P1_OUTPUT_ROOT:-${repo_root}/results/task1/p1-interleaved}"
repeats="${RT_P1_REPEATS:-3}"
max_attempts="${RT_P1_MAX_ATTEMPTS:-3}"
duration_sec="${RT_P1_DURATION_SEC:-90}"
burner_busy_ms="${RT_P1_BURNER_BUSY_MS:-10}"
burner_idle_ms="${RT_P1_BURNER_IDLE_MS:-53}"
burner_start_delay_ms="${RT_P1_BURNER_START_DELAY_MS:-60000}"
rootfs="${RT_P1_ROOTFS:-${repo_root}/tmp/axbuild/rootfs/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img}"
runner="${repo_root}/scripts/test/rt-partition/run-cyclictest.sh"
baseline_zephyr="${repo_root}/scripts/test/rt-partition/upstream-dev-zephyr.toml"

for value in "$repeats" "$max_attempts" "$duration_sec" "$burner_busy_ms" "$burner_idle_ms" "$burner_start_delay_ms"; do
    [[ "$value" =~ ^[0-9]+$ ]] || {
        printf 'error: P1 numeric option is invalid: %s\n' "$value" >&2
        exit 2
    }
done
(( repeats > 0 && max_attempts > 0 && duration_sec > 0 && burner_busy_ms > 0 && burner_idle_ms > 0 )) || {
    printf 'error: repeats, attempts, duration, and burner phases must be positive\n' >&2
    exit 2
}
for path in "$baseline_root" "$rootfs" "$runner" "$baseline_zephyr"; do
    [[ -e "$path" ]] || { printf 'error: missing P1 input: %s\n' "$path" >&2; exit 1; }
done
cmp -s \
    "$repo_root/os/axvisor/src/rt_burner.rs" \
    "$baseline_root/os/axvisor/src/rt_burner.rs" || {
    printf 'error: baseline and modified trees do not use the same RT burner implementation\n' >&2
    exit 1
}

common_env=(
    RT_DURATION_SEC="$duration_sec"
    RT_VMEXIT_DIAGNOSTICS=0
    RT_REQUIRE_INIT_DONE=0
    RT_ZEPHYR_TIMEOUT_SEC=420
    RT_PROGRESS_TIMEOUT_SEC=700
    RT_ROOTFS="$rootfs"
)
baseline_runs=()
modified_runs=()

run_with_retries() {
    local label="$1"
    shift
    local attempt
    for attempt in $(seq 1 "$max_attempts"); do
        printf 'P1_ATTEMPT_START label=%s attempt=%s\n' "$label" "$attempt"
        if "$@"; then
            printf 'P1_ATTEMPT_ACCEPTED label=%s attempt=%s\n' "$label" "$attempt"
            return 0
        fi
        printf 'P1_ATTEMPT_RETRY label=%s attempt=%s\n' "$label" "$attempt" >&2
    done
    printf 'error: P1 run failed after %s attempts: %s\n' "$max_attempts" "$label" >&2
    return 1
}

mkdir -p "$output_root"
for run_number in $(seq 1 "$repeats"); do
    run_id="$(printf 'run-%02d' "$run_number")"
    printf 'P1_RUN_START variant=baseline run=%s\n' "$run_id"
    baseline_output="$output_root/baseline/$run_id"
    run_with_retries "baseline/$run_id" env "${common_env[@]}" \
        RT_SOURCE_ROOT="$baseline_root" \
        RT_SCENARIO=stress-noiso \
        RT_BURNER="1:${burner_busy_ms}:${burner_idle_ms}:${burner_start_delay_ms}" \
        RT_ZEPHYR_TEMPLATE="$baseline_zephyr" \
        RT_OUTPUT_ROOT="$baseline_output" \
        "$runner"
    baseline_runs+=("$baseline_output/stress-noiso")

    printf 'P1_RUN_START variant=modified run=%s\n' "$run_id"
    modified_output="$output_root/modified/$run_id"
    run_with_retries "modified/$run_id" env "${common_env[@]}" \
        RT_SOURCE_ROOT="$repo_root" \
        RT_SCENARIO=stress-dedicated \
        RT_BURNER="0:${burner_busy_ms}:${burner_idle_ms}:${burner_start_delay_ms}" \
        RT_OUTPUT_ROOT="$modified_output" \
        "$runner"
    modified_runs+=("$modified_output/stress-dedicated")
done

python3 "$repo_root/scripts/test/rt-partition/compare-rt-runs.py" \
    --baseline-label upstream-dev-shared \
    --baseline "${baseline_runs[@]}" \
    --modified-label rt-dedicated \
    --modified "${modified_runs[@]}" \
    --output "$output_root/comparison.txt"

printf 'P1_COMPARISON_ACCEPTED output=%s\n' "$output_root/comparison.txt"
