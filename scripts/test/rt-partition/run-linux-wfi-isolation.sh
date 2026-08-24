#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
duration_sec="${RT_WFI_ISOLATION_DURATION_SEC:-90}"
repeats="${RT_WFI_ISOLATION_REPEATS:-1}"
output_root="${RT_WFI_ISOLATION_OUTPUT_ROOT:-${repo_root}/results/task1/linux-wfi-isolation/pilot}"
runner="${repo_root}/scripts/test/rt-partition/run-cyclictest.sh"
comparison_tool="${repo_root}/scripts/test/rt-partition/compare-rt-runs.py"
linux_kernel="${RT_WFI_ISOLATION_LINUX_KERNEL:-${repo_root}/tmp/rt-partition/linux-trace-gcc13/linux-qemu-trace}"
canonical_archive=""
last_run_dir=""

for value in "$duration_sec" "$repeats"; do
    [[ "$value" =~ ^[0-9]+$ ]] || {
        printf 'error: WFI isolation option is not numeric: %s\n' "$value" >&2
        exit 2
    }
done
(( duration_sec > 0 && repeats > 0 )) || {
    printf 'error: WFI isolation duration and repeats must be positive\n' >&2
    exit 2
}
[[ ! -e "$output_root" ]] || {
    printf 'error: RT_WFI_ISOLATION_OUTPUT_ROOT already exists: %s\n' "$output_root" >&2
    exit 2
}
[[ -f "$linux_kernel" ]] || {
    printf 'error: trace-enabled Linux kernel does not exist: %s\n' "$linux_kernel" >&2
    exit 2
}

deduplicate_archive() {
    local candidate_dir="$1"
    local file
    if [[ -z "$canonical_archive" ]]; then
        canonical_archive="$candidate_dir"
        return
    fi
    # AxVisor embeds the per-cell static VM configuration, so its binary is
    # expected to change with the timer contract and WFI policy.
    for file in linux-qemu rt-linux-initramfs.cpio.gz \
        zephyr-periodic.bin zephyr-periodic.manifest; do
        cmp -s "$canonical_archive/$file" "$candidate_dir/$file" || {
            printf 'error: WFI isolation input changed: %s\n' "$file" >&2
            exit 1
        }
        ln -f "$canonical_archive/$file" "$candidate_dir/$file"
    done
}

run_cell() {
    local cell="$1"
    local run_id="$2"
    local virtual_only="$3"
    local wfi_policy="$4"
    local run_root="${output_root}/${cell}/${run_id}"

    printf 'RT_WFI_ISOLATION_RUN_START cell=%s run=%s virtual_only=%s wfi_policy=%s\n' \
        "$cell" "$run_id" "$virtual_only" "$wfi_policy"
    RT_OUTPUT_ROOT="$run_root" \
        RT_SCENARIO=stress-dedicated \
        RT_DURATION_SEC="$duration_sec" \
        RT_LOOPS=0 \
        RT_LINUX_TRACE=timerlat \
        RT_LINUX_TRACE_BUFFER_KB=256 \
        RT_LINUX_KERNEL_OVERRIDE="$linux_kernel" \
        RT_LINUX_VIRTUAL_TIMER_ONLY="$virtual_only" \
        RT_LINUX_WFI_POLICY="$wfi_policy" \
        RT_DEDICATED_CPUS_OVERRIDE=1,2,3 \
        RT_RUNTIME_DIAGNOSTICS=1 \
        "$runner"
    last_run_dir="$run_root/stress-dedicated"
    deduplicate_archive "$last_run_dir"
    printf 'RT_WFI_ISOLATION_RUN_ACCEPTED cell=%s run=%s output=%s\n' \
        "$cell" "$run_id" "$last_run_dir"
}

mkdir -p "$output_root"
cntp_trap_runs=()
cntv_trap_runs=()
cntv_passthrough_runs=()
sequence=()
for run_number in $(seq 1 "$repeats"); do
    printf -v run_id 'run-%02d' "$run_number"
    case $(((run_number - 1) % 3)) in
        0) order=(cntp-trap cntv-trap cntv-passthrough) ;;
        1) order=(cntv-passthrough cntv-trap cntp-trap) ;;
        2) order=(cntv-trap cntp-trap cntv-passthrough) ;;
    esac
    for cell in "${order[@]}"; do
        case "$cell" in
            cntp-trap)
                run_cell "$cell" "$run_id" 0 trap
                cntp_trap_runs+=("$last_run_dir")
                ;;
            cntv-trap)
                run_cell "$cell" "$run_id" 1 trap
                cntv_trap_runs+=("$last_run_dir")
                ;;
            cntv-passthrough)
                run_cell "$cell" "$run_id" 1 passthrough
                cntv_passthrough_runs+=("$last_run_dir")
                ;;
        esac
        sequence+=("$cell")
    done
done

python3 "$comparison_tool" \
    --baseline-label cntp-exposed-trapped-wfi \
    --baseline "${cntp_trap_runs[@]}" \
    --modified-label cntv-only-trapped-wfi \
    --modified "${cntv_trap_runs[@]}" \
    --output "$output_root/timer-contract-comparison.txt"
python3 "$comparison_tool" \
    --baseline-label cntv-only-trapped-wfi \
    --baseline "${cntv_trap_runs[@]}" \
    --modified-label cntv-only-untrapped-wfi \
    --modified "${cntv_passthrough_runs[@]}" \
    --output "$output_root/wfi-path-comparison.txt"
python3 "$comparison_tool" \
    --baseline-label cntp-exposed-trapped-wfi \
    --baseline "${cntp_trap_runs[@]}" \
    --modified-label cntv-only-untrapped-wfi \
    --modified "${cntv_passthrough_runs[@]}" \
    --output "$output_root/legacy-coupled-comparison.txt"

sequence_csv="$(IFS=,; printf '%s' "${sequence[*]}")"
cat > "$output_root/protocol.txt" <<EOF
experiment=aarch64-linux-timer-contract-wfi-isolation
sequence=${sequence_csv}
repeats=${repeats}
duration_sec=${duration_sec}
scenario=stress-dedicated
dedicated_cpus=1,2,3
linux_vcpu_pcpu=0:2,1:3
cell_cntp_trap=virtual_timer_only:0,wfi_policy:trap
cell_cntv_trap=virtual_timer_only:1,wfi_policy:trap
cell_cntv_passthrough=virtual_timer_only:1,wfi_policy:passthrough
invalid_cell_omitted=virtual_timer_only:0,wfi_policy:passthrough
runtime_diagnostics=1
linux_trace=timerlat
linux_trace_buffer_kb=256
linux_kernel=${linux_kernel}
EOF

printf 'RT_WFI_ISOLATION_COMPLETE repeats=%s output=%s\n' "$repeats" "$output_root"
