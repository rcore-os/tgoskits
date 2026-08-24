#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
duration_sec="${RT_PRIORITY_AB_DURATION_SEC:-90}"
repeats="${RT_PRIORITY_AB_REPEATS:-4}"
order="${RT_PRIORITY_AB_ORDER:-counterbalanced}"
# Start the offered background load before the VMs launch. This guarantees that
# both schedulers enter the measurement window with the same runnable workload;
# fixed-priority scheduling then determines how much CPU time it receives.
burner="${RT_PRIORITY_AB_BURNER:-1:10:53}"
output_root="${RT_PRIORITY_AB_OUTPUT_ROOT:-${repo_root}/results/task1/priority-scheduler/single-variable-ab}"
runner="${repo_root}/scripts/test/rt-partition/run-cyclictest.sh"
comparison_tool="${repo_root}/scripts/test/rt-partition/compare-rt-runs.py"
rr_board="${repo_root}/scripts/test/rt-partition/board-qemu-aarch64-rr.toml"
rt_board="${repo_root}/scripts/test/rt-partition/board-qemu-aarch64-rt.toml"
common_archive=""
rr_archive=""
fixed_archive=""
last_run_dir=""

for value in "$duration_sec" "$repeats"; do
    [[ "$value" =~ ^[0-9]+$ ]] || {
        printf 'error: priority A/B option is not numeric: %s\n' "$value" >&2
        exit 2
    }
done
(( duration_sec > 0 && repeats > 0 )) || {
    printf 'error: priority A/B duration and repeats must be positive\n' >&2
    exit 2
}
case "$order" in
    counterbalanced)
        (( repeats % 2 == 0 )) || {
            printf 'error: counterbalanced priority A/B requires an even repeat count\n' >&2
            exit 2
        }
        ;;
    rr-fixed) ;;
    *)
        printf 'error: RT_PRIORITY_AB_ORDER must be counterbalanced or rr-fixed\n' >&2
        exit 2
        ;;
esac
[[ ! -e "$output_root" ]] || {
    printf 'error: RT_PRIORITY_AB_OUTPUT_ROOT already exists: %s\n' "$output_root" >&2
    exit 2
}

python3 - "$rr_board" "$rt_board" <<'PY'
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
    raise SystemExit("scheduler A/B board profiles differ outside the feature list")
if rr_features - {"rr-scheduler"} != fixed_features - {"rt-scheduler"}:
    raise SystemExit("scheduler A/B board profiles have different auxiliary features")
if "rr-scheduler" not in rr_features or "rt-scheduler" not in fixed_features:
    raise SystemExit("scheduler A/B board profiles do not select the expected schedulers")
PY

deduplicate_file() {
    local canonical="$1"
    local candidate="$2"
    [[ -f "$canonical" && -f "$candidate" ]] || {
        printf 'error: missing archive input for deduplication: %s or %s\n' \
            "$canonical" "$candidate" >&2
        exit 1
    }
    cmp -s "$canonical" "$candidate" || {
        printf 'error: single-variable A/B archive input changed: %s vs %s\n' \
            "$canonical" "$candidate" >&2
        exit 1
    }
    ln -f "$canonical" "$candidate"
}

deduplicate_archive() {
    local variant="$1"
    local candidate_dir="$2"
    local file
    if [[ -z "$common_archive" ]]; then
        common_archive="$candidate_dir"
    else
        for file in linux-qemu rt-linux-initramfs.cpio.gz \
            zephyr-periodic.bin zephyr-periodic.manifest; do
            deduplicate_file "$common_archive/$file" "$candidate_dir/$file"
        done
    fi

    case "$variant" in
        rr)
            if [[ -z "$rr_archive" ]]; then
                rr_archive="$candidate_dir"
            else
                deduplicate_file "$rr_archive/axvisor.bin" "$candidate_dir/axvisor.bin"
            fi
            ;;
        fixed-priority)
            if [[ -z "$fixed_archive" ]]; then
                fixed_archive="$candidate_dir"
            else
                deduplicate_file "$fixed_archive/axvisor.bin" "$candidate_dir/axvisor.bin"
            fi
            ;;
        *)
            printf 'error: unknown scheduler A/B variant: %s\n' "$variant" >&2
            exit 2
            ;;
    esac
}

run_case() {
    local name="$1"
    local board="$2"
    local run_id="$3"
    local run_root="${output_root}/${name}/${run_id}"
    printf 'RT_PRIORITY_AB_RUN_START variant=%s run=%s\n' "$name" "$run_id"
    RT_OUTPUT_ROOT="$run_root" \
        RT_BOARD_CONFIG="$board" \
        RT_SCENARIO=stress-noiso \
        RT_DURATION_SEC="$duration_sec" \
        RT_LOOPS=0 \
        RT_BURNER="$burner" \
        RT_RUNTIME_DIAGNOSTICS=1 \
        "$runner"
    last_run_dir="$run_root/stress-noiso"
    deduplicate_archive "$name" "$last_run_dir"
    printf 'RT_PRIORITY_AB_RUN_ACCEPTED variant=%s run=%s output=%s\n' \
        "$name" "$run_id" "$last_run_dir"
}

mkdir -p "$output_root"
rr_runs=()
fixed_runs=()
sequence=()
for run_number in $(seq 1 "$repeats"); do
    printf -v run_id 'run-%02d' "$run_number"
    if [[ "$order" == "rr-fixed" || $((run_number % 2)) -eq 1 ]]; then
        run_case "rr" "$rr_board" "$run_id"
        rr_runs+=("$last_run_dir")
        sequence+=("rr")
        run_case "fixed-priority" "$rt_board" "$run_id"
        fixed_runs+=("$last_run_dir")
        sequence+=("fixed-priority")
    else
        run_case "fixed-priority" "$rt_board" "$run_id"
        fixed_runs+=("$last_run_dir")
        sequence+=("fixed-priority")
        run_case "rr" "$rr_board" "$run_id"
        rr_runs+=("$last_run_dir")
        sequence+=("rr")
    fi
done

sequence_csv="$(IFS=,; printf '%s' "${sequence[*]}")"

python3 "$comparison_tool" \
    --baseline-label round-robin-timer89-kick91 \
    --baseline "${rr_runs[@]}" \
    --modified-label fixed-priority-timer89-kick91 \
    --modified "${fixed_runs[@]}" \
    --output "$output_root/comparison.txt"

cat > "$output_root/protocol.txt" <<EOF
experiment=fixed-priority-scheduler-single-variable-ab
order=${order}
sequence=${sequence_csv}
repeats=${repeats}
duration_sec=${duration_sec}
burner=${burner}
runtime_diagnostics=1
vcpu_priority=90
timer_worker_priority=89
rr_board=${rr_board}
fixed_board=${rt_board}
EOF

printf 'RT_PRIORITY_AB_COMPLETE repeats=%s comparison=%s\n' \
    "$repeats" "$output_root/comparison.txt"
