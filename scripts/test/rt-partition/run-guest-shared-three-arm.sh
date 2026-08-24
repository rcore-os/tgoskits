#!/usr/bin/env bash
set -euo pipefail

# Compare RR, legacy fixed-priority FIFO, and fixed-priority round-robin while
# Linux vCPU0 and Zephyr vCPU0 contend for the same physical CPU.

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
runner="${repo_root}/scripts/test/rt-partition/run-cyclictest.sh"
compare="${repo_root}/scripts/test/rt-partition/compare-rt-runs.py"
rr_board="${repo_root}/scripts/test/rt-partition/board-qemu-aarch64-rr.toml"
fixed_board="${repo_root}/scripts/test/rt-partition/board-qemu-aarch64-rt.toml"
fp_rr_board="${repo_root}/scripts/test/rt-partition/board-qemu-aarch64-fp-rr.toml"
output_root="${RT_GUEST_SHARED_THREE_ARM_OUTPUT_ROOT:-${repo_root}/results/task1/guest-shared-three-arm}"
duration_sec="${RT_GUEST_SHARED_THREE_ARM_DURATION_SEC:-60}"
repeats="${RT_GUEST_SHARED_THREE_ARM_REPEATS:-3}"
sample_count="${RT_GUEST_SHARED_THREE_ARM_SAMPLE_COUNT:-3000}"

for value in "$duration_sec" "$repeats" "$sample_count"; do
    [[ "$value" =~ ^[1-9][0-9]*$ ]] || {
        printf 'error: guest-shared-three-arm numeric option is invalid: %s\n' "$value" >&2
        exit 2
    }
done
[[ ! -e "$output_root" ]] || {
    printf 'error: output already exists: %s\n' "$output_root" >&2
    exit 2
}

python3 - "$rr_board" "$fixed_board" "$fp_rr_board" <<'PY'
import sys
import tomllib
from pathlib import Path


def load(path: str) -> dict:
    with Path(path).open("rb") as stream:
        return tomllib.load(stream)


profiles = [load(path) for path in sys.argv[1:]]
base = [dict(profile) for profile in profiles]
features = [set(profile.pop("features", [])) for profile in base]
if not all(profile == base[0] for profile in base[1:]):
    raise SystemExit("guest-shared three-arm profiles differ outside scheduler feature")
expected = [{"rr-scheduler"}, {"rt-scheduler"}, {"fp-rr-scheduler"}]
for actual, required in zip(features, expected):
    if actual - required != features[0] - {"rr-scheduler"}:
        raise SystemExit("guest-shared three-arm profiles have different auxiliary features")
    if required.isdisjoint(actual):
        raise SystemExit(f"missing scheduler feature: {required}")
PY

declare -A boards=(
    [rr]="$rr_board"
    [fixed]="$fixed_board"
    [fp-rr]="$fp_rr_board"
)
declare -A run_dirs
variants=(rr fixed fp-rr)

run_case() {
    local variant="$1" run_number="$2"
    local run_root="${output_root}/${variant}/run-$(printf '%02d' "$run_number")"
    printf 'GUEST_SHARED_THREE_ARM_RUN_START variant=%s run=%s\n' "$variant" "$run_number"
    RT_OUTPUT_ROOT="$run_root" \
        RT_BOARD_CONFIG="${boards[$variant]}" \
        RT_SCENARIO=stress-guest-shared \
        RT_CPU=0 \
        RT_LINUX_PHYS_CPU_IDS=1,2 \
        RT_ZEPHYR_PHYS_CPU_IDS=1 \
        RT_DEDICATED_CPUS_OVERRIDE= \
        RT_BURNER= \
        RT_DURATION_SEC="$duration_sec" \
        RT_LOOPS=0 \
        RT_ZEPHYR_SAMPLE_COUNT="$sample_count" \
        RT_RUNTIME_DIAGNOSTICS=1 \
        "$runner"
    run_dirs["$variant,$run_number"]="$run_root/stress-guest-shared"
    printf 'GUEST_SHARED_THREE_ARM_RUN_ACCEPTED variant=%s run=%s path=%s\n' \
        "$variant" "$run_number" "${run_dirs[$variant,$run_number]}"
}

mkdir -p "$output_root"
for run_number in $(seq 1 "$repeats"); do
    case $(((run_number - 1) % 3)) in
        0) sequence=(rr fixed fp-rr) ;;
        1) sequence=(fp-rr fixed rr) ;;
        2) sequence=(fixed rr fp-rr) ;;
    esac
    for variant in "${sequence[@]}"; do
        run_case "$variant" "$run_number"
    done
done

pairwise_compare() {
    local name="$1" baseline_variant="$2" modified_variant="$3"
    local -a baseline=() modified=()
    for run_number in $(seq 1 "$repeats"); do
        baseline+=("${run_dirs[$baseline_variant,$run_number]}")
        modified+=("${run_dirs[$modified_variant,$run_number]}")
    done
    python3 "$compare" \
        --baseline-label "$baseline_variant" --baseline "${baseline[@]}" \
        --modified-label "$modified_variant" --modified "${modified[@]}" \
        --output "$output_root/${name}.txt"
}

pairwise_compare rr-vs-fixed rr fixed
pairwise_compare rr-vs-fp-rr rr fp-rr
pairwise_compare fixed-vs-fp-rr fixed fp-rr

cat > "$output_root/protocol.txt" <<EOF
experiment=linux-zephyr-direct-shared-pcpu-three-arm
topology=linux-vcpu0->pcpu1,linux-vcpu1->pcpu2,zephyr-vcpu0->pcpu1
variable=scheduler-feature
arms=rr,fixed,fp-rr
rr_board=${rr_board}
fixed_board=${fixed_board}
fp_rr_board=${fp_rr_board}
repeats=${repeats}
duration_sec=${duration_sec}
zephyr_sample_count=${sample_count}
dedicated_cpus=none
host_burner=disabled
order=rotating-three-arm
EOF

printf 'GUEST_SHARED_THREE_ARM_COMPLETE repeats=%s output=%s\n' "$repeats" "$output_root"
