#!/usr/bin/env bash

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
workspace=$(cd -- "$script_dir/../.." && pwd)
repository_runner=$workspace/competition/ivc/orangepi/board-runner.sh
analyzer=$script_dir/analyze_board.py
metadata_writer=$script_dir/write_board_metadata.py

usage() {
    cat <<EOF
Usage: $0 [smoke|full|manual-smoke|manual-full|fault-ack-loss|fault-error] [options]

Options:
  --profile <name>        Select a normal/manual run or deterministic fault campaign.
  --repeat <count>        Run the selected profile repeatedly (default: 1).
  --board <type>          Select the board service type.
  --result-dir <path>     Store structured run results below this directory.
  --timeout <seconds>     Bound the board runner, including lease wait time.
  --restore-linux         Require Linux restoration after every board run.
  --require-clean         Reject formal evidence from a dirty Git worktree.
  -h, --help              Show this help text.
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

profile=smoke
repeat_count=1
board_type=${ORANGEPI_BOARD_TYPE:-OrangePi-5-Plus}
result_root=${ORANGEPI_IVC_RESULT_DIR:-$workspace/tmp/competition/ivc/orangepi-runs}
timeout_seconds=${ORANGEPI_RUN_TIMEOUT_SECONDS:-720}
restore_linux=1
require_clean=0

if (($# > 0)) && [[ "$1" != -* ]]; then
    profile=$1
    shift
fi
while (($# > 0)); do
    case "$1" in
        --profile)
            profile=${2:?--profile requires a value}
            shift 2
            ;;
        --repeat)
            repeat_count=${2:?--repeat requires a value}
            shift 2
            ;;
        --board)
            board_type=${2:?--board requires a value}
            shift 2
            ;;
        --result-dir)
            result_root=${2:?--result-dir requires a value}
            shift 2
            ;;
        --timeout)
            timeout_seconds=${2:?--timeout requires a value}
            shift 2
            ;;
        --restore-linux)
            restore_linux=1
            shift
            ;;
        --require-clean)
            require_clean=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "Unknown Orange Pi run option: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

analyzer_profile=normal
drop_ack_every=0
case "$profile" in
    smoke)
        build_config=competition/ivc/config/axvisor-orangepi-5-plus-smoke.toml
        board_config=competition/ivc/config/board-orangepi-5-plus-smoke.toml
        expected_count=20
        model_id=thermal-4x6x1-v1
        inference_backend=native
        guest_image_name=starry-ivc-rootfs-smoke.img
        result_image_name=ivc-ns
        ;;
    full)
        build_config=competition/ivc/config/axvisor-orangepi-5-plus.toml
        board_config=competition/ivc/config/board-orangepi-5-plus.toml
        expected_count=1800
        model_id=thermal-4x6x1-v1
        inference_backend=native
        guest_image_name=starry-ivc-rootfs.img
        result_image_name=ivc-n
        ;;
    manual-smoke)
        build_config=competition/ivc/config/axvisor-orangepi-5-plus-manual-smoke.toml
        board_config=competition/ivc/config/board-orangepi-5-plus-manual-smoke.toml
        expected_count=20
        model_id=manual-fixed-500
        inference_backend=native
        guest_image_name=starry-ivc-rootfs-manual-smoke.img
        result_image_name=ivc-ms
        ;;
    manual-full)
        build_config=competition/ivc/config/axvisor-orangepi-5-plus-manual.toml
        board_config=competition/ivc/config/board-orangepi-5-plus-manual.toml
        expected_count=1800
        model_id=manual-fixed-500
        inference_backend=native
        guest_image_name=starry-ivc-rootfs-manual.img
        result_image_name=ivc-m
        ;;
    fault-ack-loss)
        build_config=competition/ivc/config/axvisor-orangepi-5-plus-ack-loss.toml
        board_config=competition/ivc/config/board-orangepi-5-plus-ack-loss.toml
        expected_count=100
        model_id=thermal-4x6x1-v1
        inference_backend=native
        guest_image_name=starry-ivc-rootfs-ack-loss.img
        result_image_name=ivc-a
        analyzer_profile=ack-loss
        drop_ack_every=5
        ;;
    fault-error)
        build_config=competition/ivc/config/axvisor-orangepi-5-plus-error.toml
        board_config=competition/ivc/config/board-orangepi-5-plus-error.toml
        expected_count=100
        model_id=thermal-4x6x1-v1
        inference_backend=native
        guest_image_name=starry-ivc-rootfs-error.img
        result_image_name=ivc-e
        analyzer_profile=error
        ;;
    *)
        echo "Unsupported Orange Pi profile: $profile" >&2
        usage >&2
        exit 2
        ;;
esac

require_positive_integer --repeat "$repeat_count"
require_positive_integer --timeout "$timeout_seconds"
host_root=${ORANGEPI_AXVISOR_HOST_ROOT:?set ORANGEPI_AXVISOR_HOST_ROOT to the board Linux root device or PARTUUID}
guest_dir=${ORANGEPI_IVC_GUEST_DIR:-/home/orangepi/axvisor-guest}
result_dir=${ORANGEPI_IVC_RESULT_DIR:-/home/orangepi}
guest_image=$guest_dir/$guest_image_name
result_image=$result_dir/$result_image_name
artifact_dir=$workspace/tmp/competition/ivc/starry
starry_kernel=${IVC_STARRY_KERNEL:-$artifact_dir/starryos.bin}
starry_dtb=${IVC_STARRY_DTB:-$artifact_dir/starry-orangepi-5-plus.dtb}
local_rootfs=$artifact_dir/$guest_image_name
model_artifact=$workspace/tools/ivcproto/src/neural.rs

if [[ -n "${ORANGEPI_AXVISOR_RUNNER:-}" ]]; then
    runner_command=("$ORANGEPI_AXVISOR_RUNNER")
    if ! command -v "${runner_command[0]}" >/dev/null 2>&1; then
        echo "Orange Pi board runner not found: ${runner_command[0]}" >&2
        exit 1
    fi
else
    runner_command=(bash "$repository_runner")
    if [[ ! -r "$repository_runner" ]]; then
        echo "Repository Orange Pi board runner not found: $repository_runner" >&2
        exit 1
    fi
fi
for command_name in date gzip mv python3 sha256sum tee; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
        echo "Required Orange Pi result tool not found: $command_name" >&2
        exit 1
    fi
done
for input_path in "$analyzer" "$metadata_writer"; do
    if [[ ! -r "$input_path" ]]; then
        echo "Orange Pi result tool not found: $input_path" >&2
        exit 1
    fi
done
if [[ "$result_root" != /* ]]; then
    result_root=$workspace/$result_root
fi

write_checksums() {
    local run_dir=$1
    local files=(console.log console.log.gz metadata.json)

    if [[ -f "$run_dir/summary.json" ]]; then
        files+=(summary.json)
    fi
    if [[ -f "$run_dir/raw.csv" ]]; then
        files+=(raw.csv raw.csv.gz)
    fi
    (
        cd "$run_dir"
        sha256sum "${files[@]}" >checksums.sha256
    )
}

compress_evidence() {
    local source=$1
    local destination=$2

    gzip -n -c -- "$source" >"$destination.new"
    mv -f -- "$destination.new" "$destination"
}

write_legacy_result_aliases() {
    local run_dir=$1

    if ((repeat_count != 1)); then
        return
    fi
    cp -- "$run_dir/console.log" "$result_root/$profile-console.log"
    if [[ -f "$run_dir/summary.json" ]]; then
        cp -- "$run_dir/summary.json" "$result_root/$profile-summary.json"
    fi
}

cd "$workspace"
mkdir -p "$result_root/$profile"
for ((run_number = 1; run_number <= repeat_count; run_number++)); do
    printf -v run_id 'run-%03d' "$run_number"
    run_dir=$result_root/$profile/$run_id
    mkdir -p "$run_dir"
    log_path=$run_dir/console.log
    summary_path=$run_dir/summary.json
    metadata_path=$run_dir/metadata.json
    raw_csv_path=$run_dir/raw.csv
    raw_csv_gzip=$run_dir/raw.csv.gz
    console_gzip=$run_dir/console.log.gz
    rm -f -- \
        "$summary_path" \
        "$metadata_path" \
        "$raw_csv_path" \
        "$raw_csv_gzip" \
        "$console_gzip" \
        "$run_dir/checksums.sha256"

    started_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    set +e
    ORANGEPI_AXVISOR_BUILD_CONFIG=$build_config \
    ORANGEPI_AXVISOR_BOARD_CONFIG=$board_config \
    ORANGEPI_AXVISOR_HOST_ROOT=$host_root \
    ORANGEPI_AXVISOR_SHUTDOWN_MARKER_REQUIRED=1 \
    ORANGEPI_BOARD_TYPE=$board_type \
    ORANGEPI_LEASE_WAIT_SECONDS=$timeout_seconds \
    ORANGEPI_RUN_TIMEOUT_SECONDS=$timeout_seconds \
    ORANGEPI_RESTORE_LINUX=$restore_linux \
    ORANGEPI_IVC_GUEST_IMAGE=$guest_image \
    ORANGEPI_IVC_RESULT_IMAGE=$result_image \
    ORANGEPI_IVC_EXPECTED_COUNT=$expected_count \
    ORANGEPI_IVC_RAW_CSV=$raw_csv_path \
        "${runner_command[@]}" 2>&1 | tee "$log_path"
    pipeline_status=("${PIPESTATUS[@]}")
    set -e

    runner_status=${pipeline_status[0]}
    tee_status=${pipeline_status[1]}
    compression_status=0
    set +e
    compress_evidence "$log_path" "$console_gzip"
    compression_status=$?
    if [[ -f "$raw_csv_path" ]]; then
        compress_evidence "$raw_csv_path" "$raw_csv_gzip"
        raw_compression_status=$?
        if ((compression_status == 0 && raw_compression_status != 0)); then
            compression_status=$raw_compression_status
        fi
    elif ((runner_status == 0)); then
        echo "Orange Pi runner completed without harvested raw samples" >&2
        compression_status=1
    fi
    set -e
    analysis_status=0
    if ((runner_status == 0 && tee_status == 0 && compression_status == 0)); then
        set +e
        python3 "$analyzer" \
            "$console_gzip" \
            --raw-csv "$raw_csv_gzip" \
            --expected-count "$expected_count" \
            --profile "$analyzer_profile" \
            --drop-ack-every "$drop_ack_every" \
            --output "$summary_path"
        analysis_status=$?
        set -e
    fi

    final_status=$runner_status
    if ((final_status == 0 && tee_status != 0)); then
        final_status=$tee_status
    fi
    if ((final_status == 0 && analysis_status != 0)); then
        final_status=$analysis_status
    fi
    if ((final_status == 0 && compression_status != 0)); then
        final_status=$compression_status
    fi
    finished_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)

    metadata_arguments=(
        --workspace "$workspace"
        --profile "$profile"
        --run-id "$run_id"
        --run-number "$run_number"
        --repeat-count "$repeat_count"
        --board-type "$board_type"
        --started-at "$started_at"
        --finished-at "$finished_at"
        --exit-status "$final_status"
        --console-log "$console_gzip"
        --raw-csv "$raw_csv_gzip"
        --summary "$summary_path"
        --build-config "$build_config"
        --board-config "$board_config"
        --starry-kernel "$starry_kernel"
        --starry-dtb "$starry_dtb"
        --rootfs "$local_rootfs"
        --model-id "$model_id"
        --model-artifact "$model_artifact"
        --inference-backend "$inference_backend"
        --runtime-version native
        --output "$metadata_path"
    )
    if [[ "$require_clean" == 1 ]]; then
        metadata_arguments+=(--require-clean)
    fi
    python3 "$metadata_writer" "${metadata_arguments[@]}"
    write_checksums "$run_dir"
    write_legacy_result_aliases "$run_dir"

    if ((final_status != 0)); then
        echo "Orange Pi $profile $run_id failed; evidence retained in $run_dir" >&2
        exit "$final_status"
    fi
done

echo "ORANGEPI_IVC_RUNS_COMPLETE profile=$profile repeats=$repeat_count result_dir=$result_root/$profile"
