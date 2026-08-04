#!/usr/bin/env bash

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
workspace=$(cd -- "$script_dir/../.." && pwd)
repository_runner=$script_dir/orangepi/board-runner.sh
rknpu_stager=$script_dir/stage-rknpu-offline.sh
reference_analyzer=$script_dir/model/thermal_rknn_starry_reference.py
build_config=competition/ivc/config/axvisor-orangepi-5-plus-rknpu-smoke.toml
board_config=competition/ivc/config/board-orangepi-5-plus-rknpu-smoke.toml
board_type=${ORANGEPI_BOARD_TYPE:-OrangePi-5-Plus}
ssh_target=${ORANGEPI_SSH_TARGET:-orangepi@192.168.31.33}
ssh_identity=${ORANGEPI_SSH_IDENTITY:-${HOME}/.ssh/orangepi_automation}
reference_python=${RKNPU_REFERENCE_PYTHON:-/usr/bin/python3}
require_clean_source=${ORANGEPI_RKNN_REQUIRE_CLEAN_SOURCE:-0}
remote_snapshot=/home/orangepi/rknpu-result.img
result_dir=

usage() {
    cat <<EOF
Usage: $0 --result-dir PATH

Runs the AxVisor/StarryOS RK3588 NPU smoke test, restores Linux, and harvests
the immutable virtio-block snapshot, 10,000-vector raw CSV, and lifecycle
resource evidence.
EOF
}

while (($# > 0)); do
    case "$1" in
        --result-dir)
            result_dir=${2:?--result-dir requires a value}
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "Unknown RKNPU run option: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done
if [[ -z "$result_dir" ]]; then
    usage >&2
    exit 2
fi
if [[ "$result_dir" != /* ]]; then
    result_dir=$workspace/$result_dir
fi
result_dir=$(realpath -m -- "$result_dir")
if [[ -e "$result_dir" ]]; then
    echo "RKNPU result directory already exists; refusing to overwrite: $result_dir" >&2
    exit 1
fi
run_id=$(basename -- "$result_dir")
if [[ ! "$run_id" =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$ ]]; then
    echo "RKNPU result directory basename is not a valid run ID: $run_id" >&2
    exit 1
fi
case "$require_clean_source" in
    0|1) ;;
    *)
        echo "ORANGEPI_RKNN_REQUIRE_CLEAN_SOURCE must be 0 or 1" >&2
        exit 2
        ;;
esac

host_root=${ORANGEPI_AXVISOR_HOST_ROOT:?set ORANGEPI_AXVISOR_HOST_ROOT to the board Linux root device or PARTUUID}
for input_path in \
    "$repository_runner" \
    "$rknpu_stager" \
    "$reference_analyzer" \
    "$workspace/$build_config" \
    "$workspace/$board_config" \
    "$ssh_identity"; do
    if [[ ! -r "$input_path" ]]; then
        echo "Required RKNPU board-run input is not readable: $input_path" >&2
        exit 1
    fi
done
for command_name in cargo debugfs git grep mktemp mv realpath rm rsync sha256sum ssh tee; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
        echo "Required RKNPU board-run command not found: $command_name" >&2
        exit 1
    fi
done
if [[ ! -x "$reference_python" ]]; then
    echo "RKNPU reference Python is not executable: $reference_python" >&2
    exit 1
fi
if ! "$reference_python" -c 'import numpy' >/dev/null 2>&1; then
    echo "RKNPU reference Python cannot import NumPy: $reference_python" >&2
    exit 1
fi

source_commit=$(git -C "$workspace" rev-parse HEAD)
source_branch=$(git -C "$workspace" symbolic-ref --short -q HEAD || true)
if [[ -z "$source_branch" ]]; then
    source_branch=detached
fi
tracked_change_count=$(git -C "$workspace" status --short --untracked-files=no | wc -l)
untracked_file_count=$(git -C "$workspace" ls-files --others --exclude-standard | wc -l)
source_dirty=false
if ((tracked_change_count > 0 || untracked_file_count > 0)); then
    source_dirty=true
fi
if [[ "$require_clean_source" == 1 && "$source_dirty" == true ]]; then
    echo "Formal RKNPU evidence requires a clean source tree" >&2
    exit 1
fi
started_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
evidence_class=physical-board-spike
if [[ "$require_clean_source" == 1 ]]; then
    evidence_class=physical-board-formal
fi

mkdir -p "$result_dir"
console=$result_dir/console.log
stage_log=$result_dir/stage-rknpu-offline.log
snapshot=$result_dir/starry-rknpu-result.img
raw=$result_dir/raw.csv
raw_manifest=$result_dir/raw.csv.sha256
resource=$result_dir/resources.txt
resource_manifest=$result_dir/resources.txt.sha256
profile=$result_dir/rknpu-offline-profile
embedded_dir=$result_dir/embedded
built_runner=$workspace/tmp/competition/ivc/starry/thermal_rknn_starry_reference
analysis_report=$result_dir/thermal-rknn-starry-reference.json
provenance=$result_dir/run-provenance.txt
printf '%s\n' \
    'schema=1' \
    "run_id=$run_id" \
    "evidence_class=$evidence_class" \
    "source_commit=$source_commit" \
    "source_branch=$source_branch" \
    "source_dirty=$source_dirty" \
    "tracked_change_count=$tracked_change_count" \
    "untracked_file_count=$untracked_file_count" \
    "clean_source_required=$require_clean_source" \
    "started_at=$started_at" \
    >"$provenance"
ssh_options=(
    -i "$ssh_identity"
    -o IdentitiesOnly=yes
    -o BatchMode=yes
    -o StrictHostKeyChecking=accept-new
    -o ConnectTimeout=8
)
printf -v rsync_shell \
    'ssh -i %q -o IdentitiesOnly=yes -o BatchMode=yes -o StrictHostKeyChecking=accept-new -o ConnectTimeout=8' \
    "$ssh_identity"

lease_pid=
stop_lease() {
    if [[ -n "$lease_pid" ]] && kill -0 "$lease_pid" 2>/dev/null; then
        kill -TERM "$lease_pid" 2>/dev/null || true
        wait "$lease_pid" 2>/dev/null || true
    fi
    lease_pid=
}

write_checksums() {
    local checksum_partial

    checksum_partial=$(mktemp)
    if ! (
        cd "$result_dir"
        find . -type f ! -name checksums.sha256 -print0 \
            | sort -z \
            | xargs -0 -r sha256sum \
            >"$checksum_partial"
    ); then
        rm -f -- "$checksum_partial"
        return 1
    fi
    mv -- "$checksum_partial" "$result_dir/checksums.sha256"
}

cleanup() {
    local exit_status=$?
    local failure_finished_at

    stop_lease
    if ((exit_status == 0)); then
        return
    fi
    failure_finished_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    if [[ -f "$provenance" ]] && ! grep -q '^finished_at=' "$provenance"; then
        printf 'finished_at=%s\n' "$failure_finished_at" >>"$provenance"
    fi
    printf 'exit_status=%s\nfinished_at=%s\n' \
        "$exit_status" "$failure_finished_at" \
        >"$result_dir/automation-failure-status.txt"
    write_checksums || true
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

start_lease() {
    local lease_log=$1
    local lease_deadline

    cargo xtask board connect -b "$board_type" >"$lease_log" 2>&1 &
    lease_pid=$!
    lease_deadline=$((SECONDS + 60))
    while ! grep -q '^Allocated board session:' "$lease_log"; do
        if ! kill -0 "$lease_pid" 2>/dev/null; then
            echo "Board lease ended before allocation" >&2
            sed -n '1,160p' "$lease_log" >&2
            return 1
        fi
        if ((SECONDS >= lease_deadline)); then
            echo "Timed out while acquiring the $board_type lease" >&2
            return 1
        fi
        sleep 0.2
    done
}

cd "$workspace"
bash "$rknpu_stager" 2>&1 | tee "$stage_log"
start_lease "$result_dir/pre-run-board-connect.log"
ssh "${ssh_options[@]}" "$ssh_target" \
    'rm -f -- /home/orangepi/rknpu-result.img /home/orangepi/rknpu-result.img.new; sync'
stop_lease

set +e
ORANGEPI_AXVISOR_BUILD_CONFIG=$build_config \
ORANGEPI_AXVISOR_BOARD_CONFIG=$board_config \
ORANGEPI_AXVISOR_HOST_ROOT=$host_root \
ORANGEPI_AXVISOR_SHUTDOWN_MARKER_REQUIRED=1 \
ORANGEPI_BOARD_TYPE=$board_type \
ORANGEPI_RESTORE_LINUX=1 \
    bash "$repository_runner" 2>&1 | tee "$console"
pipeline_status=("${PIPESTATUS[@]}")
set -e
runner_status=${pipeline_status[0]}
tee_status=${pipeline_status[1]}
if ((runner_status != 0 || tee_status != 0)); then
    printf 'runner_status=%s\ntee_status=%s\n' \
        "$runner_status" "$tee_status" >"$result_dir/failure-status.txt"
    printf 'finished_at=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >>"$provenance"
    write_checksums
    echo "StarryOS RKNPU board run failed; evidence preserved at $result_dir" >&2
    if ((runner_status == 0)); then
        runner_status=$tee_status
    fi
    exit "$runner_status"
fi

start_lease "$result_dir/post-run-board-connect.log"
ssh "${ssh_options[@]}" "$ssh_target" sh -s -- "$remote_snapshot" \
    >"$result_dir/board-facts.txt" <<'REMOTE'
set -eu
snapshot=$1
test -s "$snapshot"
echo "hostname=$(hostname)"
echo "machine=$(uname -m)"
echo "kernel_release=$(uname -r)"
echo "root_source=$(findmnt -rn -o SOURCE /)"
echo "root_fstype=$(findmnt -rn -o FSTYPE /)"
echo "snapshot_sha256=$(sha256sum "$snapshot" | cut -d ' ' -f 1)"
echo "snapshot_size=$(stat -c %s "$snapshot")"
REMOTE
rsync -a --info=stats1 -e "$rsync_shell" \
    "$ssh_target:$remote_snapshot" "$snapshot"
ssh "${ssh_options[@]}" "$ssh_target" sync
stop_lease

debugfs -R "dump /var/lib/rknn/raw.csv $raw" "$snapshot" >/dev/null
debugfs -R "dump /var/lib/rknn/raw.csv.sha256 $raw_manifest" "$snapshot" >/dev/null
debugfs -R "dump /var/lib/rknn/resources.txt $resource" "$snapshot" >/dev/null
debugfs -R "dump /var/lib/rknn/resources.txt.sha256 $resource_manifest" \
    "$snapshot" >/dev/null
debugfs -R 'cat /etc/rknpu-offline-profile' "$snapshot" >"$profile"
mkdir -p "$embedded_dir"
debugfs -R "dump /opt/thermal-rknn/thermal_rknn_reference $embedded_dir/thermal_rknn_reference" \
    "$snapshot" >/dev/null
debugfs -R "dump /opt/thermal-rknn/thermal-4x6x1-v1-rk3588-fp16.rknn $embedded_dir/thermal-4x6x1-v1-rk3588-fp16.rknn" \
    "$snapshot" >/dev/null
debugfs -R "dump /opt/thermal-rknn/corpus.csv $embedded_dir/corpus.csv" \
    "$snapshot" >/dev/null
debugfs -R "dump /opt/thermal-rknn/lib/librknnrt.so $embedded_dir/librknnrt.so" \
    "$snapshot" >/dev/null
if [[ ! -r "$built_runner" ]]; then
    echo "Built RKNPU runner is missing after the board build: $built_runner" >&2
    exit 1
fi

raw_lines=$(wc -l <"$raw")
if [[ "$raw_lines" -ne 10001 ]]; then
    echo "Harvested RKNPU raw CSV has $raw_lines lines instead of 10001" >&2
    exit 1
fi
expected_raw_sha256=$(cut -d ' ' -f 1 "$raw_manifest")
actual_raw_sha256=$(sha256sum "$raw" | cut -d ' ' -f 1)
if [[ "$actual_raw_sha256" != "$expected_raw_sha256" ]]; then
    echo "Harvested RKNPU raw CSV hash differs from the guest manifest" >&2
    exit 1
fi
expected_resource_sha256=$(cut -d ' ' -f 1 "$resource_manifest")
actual_resource_sha256=$(sha256sum "$resource" | cut -d ' ' -f 1)
if [[ "$actual_resource_sha256" != "$expected_resource_sha256" ]]; then
    echo "Harvested RKNPU resource hash differs from the guest manifest" >&2
    exit 1
fi
for marker in \
    AXVISOR_RK3588_NPU_HANDOFF_READY \
    'IVC_RKNN_PROGRESS completed=10000' \
    THERMAL_RKNN_STARRY_PASS \
    THERMAL_RKNN_STARRY_RESOURCE \
    AXVISOR_SNAPSHOT_SYNC_OK \
    AXVISOR_HOST_FILESYSTEM_SYNCED; do
    if ! grep -aFq "$marker" "$console"; then
        echo "RKNPU console is missing required marker: $marker" >&2
        exit 1
    fi
done
if grep -aFq THERMAL_RKNN_STARRY_FAIL "$console"; then
    echo "RKNPU console contains a failure marker" >&2
    exit 1
fi

finished_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
printf 'finished_at=%s\n' "$finished_at" >>"$provenance"
analysis_args=(
    "$reference_analyzer"
    --raw "$raw"
    --raw-manifest "$raw_manifest"
    --resource "$resource"
    --resource-manifest "$resource_manifest"
    --console "$console"
    --profile "$profile"
    --board-facts "$result_dir/board-facts.txt"
    --snapshot "$snapshot"
    --embedded-runner "$embedded_dir/thermal_rknn_reference"
    --embedded-model "$embedded_dir/thermal-4x6x1-v1-rk3588-fp16.rknn"
    --embedded-corpus "$embedded_dir/corpus.csv"
    --embedded-runtime "$embedded_dir/librknnrt.so"
    --built-runner "$built_runner"
    --output "$analysis_report"
    --run-id "$run_id"
    --source-commit "$source_commit"
    --source-branch "$source_branch"
    --source-provenance captured-before-run
    --source-dirty "$source_dirty"
    --tracked-change-count "$tracked_change_count"
    --untracked-file-count "$untracked_file_count"
    --started-at "$started_at"
    --finished-at "$finished_at"
)
if [[ "$require_clean_source" == 1 ]]; then
    analysis_args+=(--require-clean-source)
fi
"$reference_python" "${analysis_args[@]}" \
    | tee "$result_dir/reference-analysis.log"
write_checksums
trap - EXIT HUP INT TERM
echo "THERMAL_RKNN_STARRY_BOARD_PASS result_dir=$result_dir vectors=10000 raw_sha256=$actual_raw_sha256 resource_sha256=$actual_resource_sha256 analysis=$analysis_report"
