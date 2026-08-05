#!/usr/bin/env bash

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
workspace=$(cd -- "$script_dir/../.." && pwd)
profile_runner=${IVC_ORT_PROFILE_RUNNER:-$script_dir/run-orangepi-5-plus.sh}
contract_writer=$script_dir/ort_campaign_contract.py
aggregator=$script_dir/aggregate_ort_campaign.py

usage() {
    cat <<EOF
Usage: $0 --result-dir <path> --expected-commit <sha> [options]

Options:
  --result-dir <path>      New campaign result root (required).
  --expected-commit <sha>  Exact clean source commit (required).
  --board <type>           Board service type (default: OrangePi-5-Plus).
  --timeout <seconds>      Per-run timeout including lease wait (default: 900).
  --dry-run                Print the frozen five-run command without execution.
  -h, --help               Show this help text.
EOF
}

require_positive_integer() {
    local name=$1
    local value=$2

    case "$value" in
        ''|*[!0-9]*)
            echo "$name must be a positive integer: $value" >&2
            exit 2
            ;;
    esac
    if ((value == 0)); then
        echo "$name must be a positive integer: $value" >&2
        exit 2
    fi
}

result_root=
expected_commit=
board_type=${ORANGEPI_BOARD_TYPE:-OrangePi-5-Plus}
timeout_seconds=${ORANGEPI_RUN_TIMEOUT_SECONDS:-900}
dry_run=0
while (($# > 0)); do
    case "$1" in
        --result-dir)
            result_root=${2:?--result-dir requires a value}
            shift 2
            ;;
        --expected-commit)
            expected_commit=${2:?--expected-commit requires a value}
            shift 2
            ;;
        --board)
            board_type=${2:?--board requires a value}
            shift 2
            ;;
        --timeout)
            timeout_seconds=${2:?--timeout requires a value}
            shift 2
            ;;
        --dry-run)
            dry_run=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "Unknown ORT campaign option: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

if [[ -z "$result_root" ]]; then
    echo "--result-dir is required" >&2
    exit 2
fi
if [[ ! "$expected_commit" =~ ^[0-9a-f]{40}$ ]]; then
    echo "--expected-commit must be a full Git SHA" >&2
    exit 2
fi
require_positive_integer --timeout "$timeout_seconds"
if [[ "$result_root" != /* ]]; then
    result_root=$workspace/$result_root
fi
if [[ -e "$result_root" ]]; then
    echo "Refusing to reuse ORT campaign result root: $result_root" >&2
    exit 73
fi
for tool in git python3 sha256sum; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "Required ORT campaign tool not found: $tool" >&2
        exit 1
    fi
done
for tool_path in "$profile_runner" "$contract_writer" "$aggregator"; do
    if [[ ! -r "$tool_path" ]]; then
        echo "ORT campaign tool is not readable: $tool_path" >&2
        exit 1
    fi
done
if [[ ! -x "$profile_runner" ]]; then
    echo "ORT profile runner is not executable: $profile_runner" >&2
    exit 1
fi

observed_commit=$(git -C "$workspace" rev-parse HEAD)
if [[ "$observed_commit" != "$expected_commit" ]]; then
    echo "ORT campaign source commit differs from --expected-commit" >&2
    exit 1
fi
branch=$(git -C "$workspace" branch --show-current)

build_config=${IVC_ORT_BUILD_CONFIG:-$workspace/competition/ivc/config/axvisor-orangepi-5-plus-ort-control.toml}
board_config=${IVC_ORT_BOARD_CONFIG:-$workspace/competition/ivc/config/board-orangepi-5-plus-ort-control.toml}
artifact_dir=$workspace/tmp/competition/ivc/starry
rootfs=${IVC_ORT_ROOTFS:-$artifact_dir/starry-ivc-rootfs-ort-control.img}
starry_kernel=${IVC_STARRY_KERNEL:-$artifact_dir/starryos.bin}
starry_dtb=${IVC_STARRY_DTB:-$artifact_dir/starry-orangepi-5-plus.dtb}
zephyr_guest=${IVC_ORT_ZEPHYR_GUEST:-$workspace/competition/ivc/zephyr/build-board/zephyr/zephyr.bin}
model_artifact=${IVC_ORT_MODEL_ARTIFACT:-$workspace/competition/ivc/model/thermal-4x6x1-v1.ort}
profile_root=$result_root/ort-full
runner_command=(
    "$profile_runner"
    ort-full
    --repeat 5
    --board "$board_type"
    --result-dir "$result_root"
    --timeout "$timeout_seconds"
    --restore-linux
    --require-clean
)

if ((dry_run == 1)); then
    printf 'ORT_CAMPAIGN_DRY_RUN commit=%s runs=5 samples_per_run=1800\n' \
        "$expected_commit"
    printf 'ORT_CAMPAIGN_DRY_RUN_COMMAND'
    printf ' %q' "${runner_command[@]}"
    printf '\n'
    echo "ORT_CAMPAIGN_DRY_RUN_COMPLETE result_root=$result_root"
    exit 0
fi

if [[ -n "$(git -C "$workspace" status --porcelain=v1)" ]]; then
    echo "formal ORT campaign requires a clean Git worktree" >&2
    exit 1
fi
: "${ORANGEPI_AXVISOR_HOST_ROOT:?set ORANGEPI_AXVISOR_HOST_ROOT for the board Linux root}"
: "${TGOS_BOARD_POWER_CONFIG:?set TGOS_BOARD_POWER_CONFIG for Linux restoration}"
: "${ORANGEPI_POWER_PYTHON:?set ORANGEPI_POWER_PYTHON for Linux restoration}"
if [[ ! -r "$TGOS_BOARD_POWER_CONFIG" ]]; then
    echo "Board power config is not readable: $TGOS_BOARD_POWER_CONFIG" >&2
    exit 1
fi
if [[ ! -x "$ORANGEPI_POWER_PYTHON" ]]; then
    echo "Board power Python is not executable: $ORANGEPI_POWER_PYTHON" >&2
    exit 1
fi
for artifact in \
    "$build_config" \
    "$board_config" \
    "$rootfs" \
    "$starry_kernel" \
    "$starry_dtb" \
    "$zephyr_guest" \
    "$model_artifact"; do
    if [[ ! -f "$artifact" ]]; then
        echo "Required ORT campaign artifact is missing: $artifact" >&2
        exit 1
    fi
done

mkdir -p "$profile_root"
python3 "$contract_writer" \
    --workspace "$workspace" \
    --expected-commit "$expected_commit" \
    --branch "$branch" \
    --board-config "$board_config" \
    --build-config "$build_config" \
    --rootfs "$rootfs" \
    --starry-dtb "$starry_dtb" \
    --starry-kernel "$starry_kernel" \
    --zephyr-guest "$zephyr_guest" \
    --model-artifact "$model_artifact" \
    --output "$profile_root/preregistration.json"
(
    cd "$profile_root"
    sha256sum preregistration.json >preregistration.sha256
)
echo "ORT_CAMPAIGN_PREREGISTERED path=$profile_root/preregistration.json"

"${runner_command[@]}"
python3 "$aggregator" \
    "$profile_root" \
    --expected-commit "$expected_commit" \
    --output "$profile_root/campaign-summary.json"
(
    cd "$profile_root"
    campaign_files=(
        preregistration.json
        preregistration.sha256
        campaign-summary.json
        run-{001..005}/checksums.sha256
    )
    sha256sum "${campaign_files[@]}" >campaign-checksums.sha256
    sha256sum --check campaign-checksums.sha256
)
echo "ORT_CAMPAIGN_COMPLETE runs=5 samples=9000 result_root=$profile_root"
