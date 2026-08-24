#!/usr/bin/env bash
set -euo pipefail

# Compare RR and fixed-priority scheduling while Linux vCPU0 and Zephyr vCPU0
# contend for the same physical CPU. The topology is held constant; only the
# AxVisor scheduler feature changes between the two board profiles.

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
runner="${repo_root}/scripts/test/rt-partition/run-cyclictest.sh"
compare="${repo_root}/scripts/test/rt-partition/compare-rt-runs.py"
rr_board="${repo_root}/scripts/test/rt-partition/board-qemu-aarch64-rr.toml"
fixed_board="${repo_root}/scripts/test/rt-partition/board-qemu-aarch64-rt.toml"
output_root="${RT_GUEST_SHARED_OUTPUT_ROOT:-${repo_root}/results/task1/guest-shared-ab}"
duration_sec="${RT_GUEST_SHARED_DURATION_SEC:-60}"
repeats="${RT_GUEST_SHARED_REPEATS:-3}"
sample_count="${RT_GUEST_SHARED_SAMPLE_COUNT:-300}"

for value in "$duration_sec" "$repeats" "$sample_count"; do
    [[ "$value" =~ ^[1-9][0-9]*$ ]] || {
        printf 'error: guest-shared numeric option is invalid: %s\n' "$value" >&2
        exit 2
    }
done
[[ ! -e "$output_root" ]] || {
    printf 'error: output already exists: %s\n' "$output_root" >&2
    exit 2
}

python3 - "$rr_board" "$fixed_board" <<'PY'
import sys
import tomllib
from pathlib import Path


def load(path: str) -> dict:
    with Path(path).open("rb") as stream:
        return tomllib.load(stream)


rr = load(sys.argv[1])
fixed = load(sys.argv[2])
rr_features = set(rr.pop("features", []))
fixed_features = set(fixed.pop("features", []))
if rr != fixed:
    raise SystemExit("guest-shared A/B board profiles differ outside the scheduler feature")
if rr_features - {"rr-scheduler"} != fixed_features - {"rt-scheduler"}:
    raise SystemExit("guest-shared A/B board profiles have different auxiliary features")
if "rr-scheduler" not in rr_features or "rt-scheduler" not in fixed_features:
    raise SystemExit("guest-shared A/B board profiles do not select RR and fixed-priority schedulers")
PY

run_case() {
    local variant="$1" board="$2" run_number="$3"
    local run_root="${output_root}/${variant}/run-$(printf '%02d' "$run_number")"
    printf 'GUEST_SHARED_RUN_START variant=%s run=%s\n' "$variant" "$run_number"
    RT_OUTPUT_ROOT="$run_root" \
        RT_BOARD_CONFIG="$board" \
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
        RT_HOLD_AFTER_COMPLETE=1 \
        "$runner"
    printf 'GUEST_SHARED_RUN_ACCEPTED variant=%s run=%s path=%s\n' \
        "$variant" "$run_number" "$run_root/stress-guest-shared"
}

mkdir -p "$output_root"
rr_runs=()
fixed_runs=()
sequence=()
for run_number in $(seq 1 "$repeats"); do
    if (( run_number % 2 == 1 )); then
        run_case rr "$rr_board" "$run_number"
        rr_runs+=("$output_root/rr/run-$(printf '%02d' "$run_number")/stress-guest-shared")
        sequence+=(rr)
        run_case fixed "$fixed_board" "$run_number"
        fixed_runs+=("$output_root/fixed/run-$(printf '%02d' "$run_number")/stress-guest-shared")
        sequence+=(fixed)
    else
        run_case fixed "$fixed_board" "$run_number"
        fixed_runs+=("$output_root/fixed/run-$(printf '%02d' "$run_number")/stress-guest-shared")
        sequence+=(fixed)
        run_case rr "$rr_board" "$run_number"
        rr_runs+=("$output_root/rr/run-$(printf '%02d' "$run_number")/stress-guest-shared")
        sequence+=(rr)
    fi
done

python3 "$compare" \
    --baseline-label guest-shared-rr --baseline "${rr_runs[@]}" \
    --modified-label guest-shared-fixed --modified "${fixed_runs[@]}" \
    --output "$output_root/comparison.txt"

sequence_csv="$(IFS=,; printf '%s' "${sequence[*]}")"
cat > "$output_root/protocol.txt" <<EOF
experiment=linux-zephyr-direct-shared-pcpu-ab
topology=linux-vcpu0->pcpu1,linux-vcpu1->pcpu2,zephyr-vcpu0->pcpu1
variable=scheduler-feature
rr_board=${rr_board}
fixed_board=${fixed_board}
sequence=${sequence_csv}
repeats=${repeats}
duration_sec=${duration_sec}
zephyr_sample_count=${sample_count}
dedicated_cpus=none
host_burner=disabled
EOF

printf 'GUEST_SHARED_COMPLETE repeats=%s comparison=%s\n' \
    "$repeats" "$output_root/comparison.txt"
