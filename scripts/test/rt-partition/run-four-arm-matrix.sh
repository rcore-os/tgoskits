#!/usr/bin/env bash
set -euo pipefail

# Run the minimum Task1 separation matrix:
# shared/partitioned topology x round-robin/fixed-priority scheduler.

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
runner="${repo_root}/scripts/test/rt-partition/run-cyclictest.sh"
compare="${repo_root}/scripts/test/rt-partition/compare-rt-runs.py"
rr_board="${repo_root}/scripts/test/rt-partition/board-qemu-aarch64-rr.toml"
fixed_board="${repo_root}/scripts/test/rt-partition/board-qemu-aarch64-rt.toml"
output_root="${RT_FOUR_ARM_OUTPUT_ROOT:-${repo_root}/results/task1/four-arm-matrix}"
duration_sec="${RT_FOUR_ARM_DURATION_SEC:-60}"
repeats="${RT_FOUR_ARM_REPEATS:-3}"
sample_count="${RT_FOUR_ARM_SAMPLE_COUNT:-3000}"
burner_busy="${RT_FOUR_ARM_BURNER_BUSY_MS:-10}"
burner_idle="${RT_FOUR_ARM_BURNER_IDLE_MS:-53}"

for value in "$duration_sec" "$repeats" "$sample_count" "$burner_busy" "$burner_idle"; do
    [[ "$value" =~ ^[1-9][0-9]*$ ]] || {
        printf 'error: four-arm numeric option is invalid: %s\n' "$value" >&2
        exit 2
    }
done
[[ ! -e "$output_root" ]] || {
    printf 'error: output already exists: %s\n' "$output_root" >&2
    exit 2
}

mkdir -p "$output_root"
declare -A run_dirs
arms=(shared-rr shared-fixed partition-rr partition-fixed)

run_arm() {
    local arm="$1" run_number="$2" board dedicated burner arm_dir
    case "$arm" in
        shared-rr) board="$rr_board"; dedicated=""; burner="1:${burner_busy}:${burner_idle}" ;;
        shared-fixed) board="$fixed_board"; dedicated=""; burner="1:${burner_busy}:${burner_idle}" ;;
        partition-rr) board="$rr_board"; dedicated="1"; burner="0:${burner_busy}:${burner_idle}" ;;
        partition-fixed) board="$fixed_board"; dedicated="1"; burner="0:${burner_busy}:${burner_idle}" ;;
        *) printf 'error: unknown arm %s\n' "$arm" >&2; exit 2 ;;
    esac
    arm_dir="${output_root}/${arm}/run-$(printf '%02d' "$run_number")"
    printf 'FOUR_ARM_START arm=%s run=%s\n' "$arm" "$run_number"
    RT_OUTPUT_ROOT="$arm_dir" \
    RT_BOARD_CONFIG="$board" \
    RT_SCENARIO=stress-noiso \
    RT_DURATION_SEC="$duration_sec" \
    RT_LOOPS=0 \
    RT_BURNER="$burner" \
    RT_DEDICATED_CPUS_OVERRIDE="$dedicated" \
    RT_ZEPHYR_SAMPLE_COUNT="$sample_count" \
    RT_RUNTIME_DIAGNOSTICS=1 \
    "$runner"
    run_dirs["$arm,$run_number"]="${arm_dir}/stress-noiso"
    printf 'FOUR_ARM_ACCEPTED arm=%s run=%s path=%s\n' "$arm" "$run_number" "${run_dirs[$arm,$run_number]}"
}

for run_number in $(seq 1 "$repeats"); do
    if (( run_number % 2 == 1 )); then
        sequence=(shared-rr shared-fixed partition-rr partition-fixed)
    else
        sequence=(partition-fixed partition-rr shared-fixed shared-rr)
    fi
    for arm in "${sequence[@]}"; do
        run_arm "$arm" "$run_number"
    done
done

pairwise_compare() {
    local name="$1" baseline_arm="$2" modified_arm="$3"
    local -a baseline modified
    for run_number in $(seq 1 "$repeats"); do
        baseline+=("${run_dirs[$baseline_arm,$run_number]}")
        modified+=("${run_dirs[$modified_arm,$run_number]}")
    done
    python3 "$compare" \
        --baseline-label "$baseline_arm" --baseline "${baseline[@]}" \
        --modified-label "$modified_arm" --modified "${modified[@]}" \
        --output "$output_root/${name}.txt"
}

pairwise_compare shared-scheduler shared-rr shared-fixed
pairwise_compare partition-effect shared-rr partition-rr
pairwise_compare partitioned-scheduler partition-rr partition-fixed
pairwise_compare shared-vs-partition shared-fixed partition-fixed

cat > "$output_root/protocol.txt" <<EOF
experiment=task1-four-arm-separation
arms=shared-rr,shared-fixed,partition-rr,partition-fixed
duration_sec=${duration_sec}
repeats=${repeats}
zephyr_sample_count=${sample_count}
shared_burner=1:${burner_busy}:${burner_idle}
partition_burner=0:${burner_busy}:${burner_idle}
order=shared-rr,shared-fixed,partition-rr,partition-fixed / partition-fixed,partition-rr,shared-fixed,shared-rr
EOF

printf 'FOUR_ARM_COMPLETE output=%s\n' "$output_root"
