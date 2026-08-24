#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
duration_sec="${RT_EXIT_YIELD_AB_DURATION_SEC:-30}"
repeats="${RT_EXIT_YIELD_AB_REPEATS:-1}"
output_root="${RT_EXIT_YIELD_AB_OUTPUT_ROOT:-${repo_root}/results/task1/vcpu-exit-yield/pilot}"
runner="${repo_root}/scripts/test/rt-partition/run-cyclictest.sh"
comparison_tool="${repo_root}/scripts/test/rt-partition/compare-rt-runs.py"
mechanism_summary_tool="${repo_root}/scripts/test/rt-partition/summarize-vcpu-exit-yield-ab.py"
linux_kernel="${RT_EXIT_YIELD_AB_LINUX_KERNEL:-${repo_root}/tmp/rt-partition/linux-trace-gcc13/linux-qemu-trace}"
baseline_board="${repo_root}/scripts/test/rt-partition/board-qemu-aarch64-rt.toml"
modified_board="${repo_root}/scripts/test/rt-partition/board-qemu-aarch64-rt-no-exit-yield.toml"
canonical_archive=""
last_run_dir=""

for value in "$duration_sec" "$repeats"; do
    [[ "$value" =~ ^[0-9]+$ ]] || {
        printf 'error: exit-yield A/B option is not numeric: %s\n' "$value" >&2
        exit 2
    }
done
(( duration_sec > 0 && repeats > 0 )) || {
    printf 'error: exit-yield A/B duration and repeats must be positive\n' >&2
    exit 2
}
[[ ! -e "$output_root" ]] || {
    printf 'error: RT_EXIT_YIELD_AB_OUTPUT_ROOT already exists: %s\n' "$output_root" >&2
    exit 2
}
[[ -f "$linux_kernel" ]] || {
    printf 'error: trace-enabled Linux kernel does not exist: %s\n' "$linux_kernel" >&2
    exit 2
}

python3 - "$baseline_board" "$modified_board" <<'PY'
import sys
import tomllib
from pathlib import Path

baseline = tomllib.loads(Path(sys.argv[1]).read_text())
modified = tomllib.loads(Path(sys.argv[2]).read_text())
baseline_features = set(baseline.pop("features", []))
modified_features = set(modified.pop("features", []))
if baseline != modified:
    raise SystemExit("exit-yield A/B board profiles differ outside features")
if modified_features - baseline_features != {"no-vcpu-exit-yield"}:
    raise SystemExit("modified board must add only no-vcpu-exit-yield")
if baseline_features - modified_features:
    raise SystemExit("modified board removed baseline features")
PY

deduplicate_archive() {
    local candidate_dir="$1"
    local file
    if [[ -z "$canonical_archive" ]]; then
        canonical_archive="$candidate_dir"
        return
    fi
    for file in linux-qemu rt-linux-initramfs.cpio.gz \
        zephyr-periodic.bin zephyr-periodic.manifest; do
        cmp -s "$canonical_archive/$file" "$candidate_dir/$file" || {
            printf 'error: exit-yield A/B input changed: %s\n' "$file" >&2
            exit 1
        }
        ln -f "$canonical_archive/$file" "$candidate_dir/$file"
    done
}

run_cell() {
    local cell="$1"
    local run_id="$2"
    local board="$3"
    local run_root="${output_root}/${cell}/${run_id}"

    printf 'RT_EXIT_YIELD_RUN_START cell=%s run=%s board=%s\n' "$cell" "$run_id" "$board"
    RT_OUTPUT_ROOT="$run_root" \
        RT_BOARD_CONFIG="$board" \
        RT_SCENARIO=stress-dedicated \
        RT_DURATION_SEC="$duration_sec" \
        RT_LOOPS=0 \
        RT_LINUX_TRACE=timerlat \
        RT_LINUX_TRACE_BUFFER_KB=256 \
        RT_LINUX_KERNEL_OVERRIDE="$linux_kernel" \
        RT_LINUX_VIRTUAL_TIMER_ONLY=1 \
        RT_LINUX_WFI_POLICY=trap \
        RT_DEDICATED_CPUS_OVERRIDE=1,2,3 \
        RT_RUNTIME_DIAGNOSTICS=1 \
        "$runner"
    last_run_dir="$run_root/stress-dedicated"
    deduplicate_archive "$last_run_dir"
    printf 'RT_EXIT_YIELD_RUN_ACCEPTED cell=%s run=%s output=%s\n' \
        "$cell" "$run_id" "$last_run_dir"
}

mkdir -p "$output_root"
baseline_runs=()
modified_runs=()
sequence=()
for run_number in $(seq 1 "$repeats"); do
    printf -v run_id 'run-%02d' "$run_number"
    if (( run_number % 2 == 1 )); then
        order=(baseline modified)
    else
        order=(modified baseline)
    fi
    for cell in "${order[@]}"; do
        if [[ "$cell" == "baseline" ]]; then
            run_cell "$cell" "$run_id" "$baseline_board"
            baseline_runs+=("$last_run_dir")
        else
            run_cell "$cell" "$run_id" "$modified_board"
            modified_runs+=("$last_run_dir")
        fi
        sequence+=("$cell")
    done
done

python3 "$comparison_tool" \
    --baseline-label post-vmexit-yield \
    --baseline "${baseline_runs[@]}" \
    --modified-label no-post-vmexit-yield \
    --modified "${modified_runs[@]}" \
    --output "$output_root/comparison.txt"

python3 "$mechanism_summary_tool" \
    --baseline "${baseline_runs[@]}" \
    --modified "${modified_runs[@]}" \
    --vcpu-id 1 \
    --output "$output_root/mechanism-comparison.txt"

sequence_csv="$(IFS=,; printf '%s' "${sequence[*]}")"
cat > "$output_root/protocol.txt" <<EOF
experiment=post-vmexit-yield-ab
sequence=${sequence_csv}
repeats=${repeats}
duration_sec=${duration_sec}
scenario=stress-dedicated
dedicated_cpus=1,2,3
linux_vcpu_pcpu=0:2,1:3
timer_contract=cntv-only
wfi_policy=trap
baseline_board=${baseline_board}
modified_board=${modified_board}
runtime_diagnostics=1
linux_trace=timerlat
linux_trace_buffer_kb=256
linux_kernel=${linux_kernel}
EOF

printf 'RT_EXIT_YIELD_AB_COMPLETE repeats=%s output=%s\n' "$repeats" "$output_root"
