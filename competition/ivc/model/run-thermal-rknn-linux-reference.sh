#!/usr/bin/env bash

set -Eeuo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
workspace=$(cd -- "$script_dir/../../.." && pwd)
board_type=${ORANGEPI_BOARD_TYPE:-OrangePi-5-Plus}
ssh_target=${ORANGEPI_SSH_TARGET:-orangepi@192.168.31.33}
ssh_identity=${ORANGEPI_SSH_IDENTITY:-${HOME}/.ssh/orangepi_automation}
rknn_python=${IVC_RKNN_PYTHON:-python3}
result_dir=
run_id=
require_clean=0

usage() {
    cat <<'USAGE'
Usage: run-thermal-rknn-linux-reference.sh \
  --result-dir PATH --run-id ID [--require-clean]

Environment:
  IVC_RKNN_PYTHON       Python with the frozen NumPy dependencies
  ORANGEPI_BOARD_TYPE   Local board-service type (default: OrangePi-5-Plus)
  ORANGEPI_SSH_TARGET   Linux SSH target
  ORANGEPI_SSH_IDENTITY Linux SSH private key
USAGE
}

while (($# > 0)); do
    case "$1" in
        --result-dir)
            result_dir=${2:?missing value for --result-dir}
            shift 2
            ;;
        --run-id)
            run_id=${2:?missing value for --run-id}
            shift 2
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
            echo "Unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

if [[ -z "$result_dir" || -z "$run_id" ]]; then
    usage >&2
    exit 2
fi
if [[ ! "$run_id" =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$ ]]; then
    echo "Run ID contains unsupported characters: $run_id" >&2
    exit 2
fi
if [[ "$result_dir" != /* ]]; then
    result_dir=$workspace/$result_dir
fi
result_dir=$(realpath -m -- "$result_dir")
if [[ -e "$result_dir" ]]; then
    echo "Result path already exists; refusing to overwrite evidence: $result_dir" >&2
    exit 1
fi

cd "$workspace"
source_commit=$(git rev-parse HEAD)
source_branch=$(git branch --show-current)
if [[ -z "$source_branch" ]]; then
    source_branch=DETACHED
fi
mapfile -t tracked_changes < <(
    git status --porcelain=v1 --untracked-files=no --ignore-submodules=all
)
mapfile -t untracked_files < <(git ls-files --others --exclude-standard)
tracked_change_count=${#tracked_changes[@]}
untracked_file_count=${#untracked_files[@]}
source_dirty=false
if ((tracked_change_count > 0 || untracked_file_count > 0)); then
    source_dirty=true
fi
if ((require_clean == 1)) && [[ "$source_dirty" == true ]]; then
    echo "Formal RKNN Linux evidence requires a clean Git worktree" >&2
    echo "tracked_changes=$tracked_change_count untracked_files=$untracked_file_count" >&2
    exit 1
fi

analyzer=$script_dir/thermal_rknn_linux_reference.py
runner_source=$script_dir/thermal_rknn_linux_reference.cpp
model=$script_dir/thermal-4x6x1-v1-rk3588-fp16.rknn
rknn_header=$workspace/apps/starry/orangepi-5-plus-uvc-rknn/rknn-yolov8-image/3rdparty/rknpu2/include/rknn_api.h
rknn_runtime=$workspace/apps/starry/orangepi-5-plus-uvc-rknn/rknn-yolov8-image/3rdparty/rknpu2/Linux/aarch64/librknnrt.so
for input_path in \
    "$analyzer" \
    "$runner_source" \
    "$model" \
    "$rknn_header" \
    "$rknn_runtime" \
    "$ssh_identity"; do
    if [[ ! -r "$input_path" ]]; then
        echo "Required RKNN Linux reference input is not readable: $input_path" >&2
        exit 1
    fi
done
for command_name in cargo cut date find g++ git grep mktemp mv realpath rsync sha256sum sort ssh tee xargs; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
        echo "Required RKNN Linux reference tool is missing: $command_name" >&2
        exit 1
    fi
done
if ! "$rknn_python" -c 'import numpy' >/dev/null 2>&1; then
    echo "IVC_RKNN_PYTHON cannot import NumPy: $rknn_python" >&2
    exit 1
fi

mkdir -p -- "$(dirname -- "$result_dir")"
mkdir -- "$result_dir"
corpus=$result_dir/corpus.csv
"$rknn_python" "$analyzer" prepare --output "$corpus" >/dev/null

runtime_sha256=$(sha256sum "$rknn_runtime" | cut -d ' ' -f 1)
rknn_sha256=$(sha256sum "$model" | cut -d ' ' -f 1)
corpus_sha256=$(sha256sum "$corpus" | cut -d ' ' -f 1)
remote_dir=/home/orangepi/ivc-rknn-reference-$run_id
started_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)

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
cleanup() {
    local status=$?
    trap - EXIT HUP INT TERM
    if [[ -n "$lease_pid" ]] && kill -0 "$lease_pid" 2>/dev/null; then
        ssh -n "${ssh_options[@]}" "$ssh_target" sync >/dev/null 2>&1 || true
        kill -TERM "$lease_pid" 2>/dev/null || true
        wait "$lease_pid" 2>/dev/null || true
    fi
    exit "$status"
}
trap cleanup EXIT HUP INT TERM

write_checksums() {
    local checksum_temp

    checksum_temp=$(mktemp "${TMPDIR:-/tmp}/ivc-rknn-checksums.XXXXXX")
    (
        cd "$result_dir"
        find . -type f ! -name checksums.sha256 -print0 \
            | sort -z \
            | xargs -0 sha256sum
    ) >"$checksum_temp"
    mv -f -- "$checksum_temp" "$result_dir/checksums.sha256"
}

cargo xtask board connect -b "$board_type" >"$result_dir/board-connect.log" 2>&1 &
lease_pid=$!
lease_deadline=$((SECONDS + 60))
while ! grep -q '^Allocated board session:' "$result_dir/board-connect.log"; do
    if ! kill -0 "$lease_pid" 2>/dev/null; then
        echo "Board lease ended before allocation" >&2
        sed -n '1,160p' "$result_dir/board-connect.log" >&2
        exit 1
    fi
    if ((SECONDS >= lease_deadline)); then
        echo "Timed out while acquiring the $board_type lease" >&2
        sed -n '1,160p' "$result_dir/board-connect.log" >&2
        exit 1
    fi
    sleep 0.2
done

if ! ssh -n "${ssh_options[@]}" "$ssh_target" test ! -e "$remote_dir"; then
    echo "Remote evidence path already exists; choose a new run ID: $remote_dir" >&2
    exit 1
fi
ssh -n "${ssh_options[@]}" "$ssh_target" mkdir -p -- "$remote_dir/include" "$remote_dir/lib"
rsync -a -e "$rsync_shell" "$rknn_header" "$ssh_target:$remote_dir/include/rknn_api.h"
rsync -a -e "$rsync_shell" "$rknn_runtime" "$ssh_target:$remote_dir/lib/librknnrt.so"
rsync -a -e "$rsync_shell" "$runner_source" "$ssh_target:$remote_dir/thermal_rknn_linux_reference.cpp"
rsync -a -e "$rsync_shell" "$model" "$ssh_target:$remote_dir/thermal-4x6x1-v1-rk3588-fp16.rknn"
rsync -a -e "$rsync_shell" "$corpus" "$ssh_target:$remote_dir/corpus.csv"

set +e
ssh "${ssh_options[@]}" "$ssh_target" sh -s -- \
    "$remote_dir" "$runtime_sha256" "$rknn_sha256" "$corpus_sha256" <<'REMOTE'
set -u
deploy=$1
expected_runtime_sha256=$2
expected_rknn_sha256=$3
expected_corpus_sha256=$4

actual_runtime_sha256=$(sha256sum "$deploy/lib/librknnrt.so" | cut -d ' ' -f 1)
actual_rknn_sha256=$(sha256sum "$deploy/thermal-4x6x1-v1-rk3588-fp16.rknn" | cut -d ' ' -f 1)
actual_corpus_sha256=$(sha256sum "$deploy/corpus.csv" | cut -d ' ' -f 1)
if [ "$actual_runtime_sha256" != "$expected_runtime_sha256" ] || \
   [ "$actual_rknn_sha256" != "$expected_rknn_sha256" ] || \
   [ "$actual_corpus_sha256" != "$expected_corpus_sha256" ]; then
    echo "deployed artifact hash mismatch" >"$deploy/deployment-error.log"
    sync
    exit 1
fi

cpu_temp_start=$(cat /sys/class/thermal/thermal_zone0/temp)
g++ -std=c++17 -O2 -Wall -Wextra -Werror \
    -I"$deploy/include" \
    "$deploy/thermal_rknn_linux_reference.cpp" \
    -L"$deploy/lib" -Wl,-rpath,'$ORIGIN/lib' \
    -lrknnrt -ldl -lpthread \
    -o "$deploy/thermal_rknn_linux_reference" \
    >"$deploy/build.log" 2>&1
build_status=$?
if [ "$build_status" -ne 0 ]; then
    sync
    exit "$build_status"
fi

ldd "$deploy/thermal_rknn_linux_reference" >"$deploy/ldd.log" 2>&1
readelf -d "$deploy/thermal_rknn_linux_reference" >"$deploy/readelf-dynamic.log" 2>&1
if ! grep -Fq "librknnrt.so => $deploy/lib/librknnrt.so " "$deploy/ldd.log"; then
    echo "runner did not resolve the deployed RKNN Runtime" >"$deploy/deployment-error.log"
    sync
    exit 1
fi

LD_LIBRARY_PATH="$deploy/lib" "$deploy/thermal_rknn_linux_reference" \
    --model "$deploy/thermal-4x6x1-v1-rk3588-fp16.rknn" \
    --corpus "$deploy/corpus.csv" \
    --output "$deploy/raw.csv.partial" \
    --warmup 32 \
    --core-mask 0 \
    >"$deploy/console.log.partial" 2>&1
run_status=$?
if [ "$run_status" -ne 0 ]; then
    sync
    exit "$run_status"
fi
mv -f -- "$deploy/raw.csv.partial" "$deploy/raw.csv"
mv -f -- "$deploy/console.log.partial" "$deploy/console.log"

cpu_temp_finish=$(cat /sys/class/thermal/thermal_zone0/temp)
runner_sha256=$(sha256sum "$deploy/thermal_rknn_linux_reference" | cut -d ' ' -f 1)
machine_id_sha256=$(sha256sum /etc/machine-id | cut -d ' ' -f 1)
gxx_version=$(g++ --version | sed -n '1p')
gxx_version_hex=$(printf '%s' "$gxx_version" | od -An -tx1 | tr -d ' \n')
{
    echo 'schema=1'
    echo "hostname=$(hostname)"
    echo "machine=$(uname -m)"
    echo "kernel_release=$(uname -r)"
    echo "rknpu_version=$(cat /sys/module/rknpu/version)"
    echo "root_source=$(findmnt -rn -o SOURCE /)"
    echo "root_fstype=$(findmnt -rn -o FSTYPE /)"
    echo "root_options=$(findmnt -rn -o OPTIONS /)"
    echo "machine_id_sha256=$machine_id_sha256"
    echo "cpu_temp_start_milli_c=$cpu_temp_start"
    echo "cpu_temp_finish_milli_c=$cpu_temp_finish"
    echo "gxx_version_hex=$gxx_version_hex"
    echo "runtime_sha256=$actual_runtime_sha256"
    echo "rknn_sha256=$actual_rknn_sha256"
    echo "corpus_sha256=$actual_corpus_sha256"
    echo "runner_sha256=$runner_sha256"
} >"$deploy/board-facts.txt"
sync
REMOTE
remote_status=$?
set -e

mkdir -- "$result_dir/board"
rsync -a --exclude='lib/librknnrt.so' -e "$rsync_shell" \
    "$ssh_target:$remote_dir/" "$result_dir/board/"
ssh -n "${ssh_options[@]}" "$ssh_target" sync
kill -TERM "$lease_pid" 2>/dev/null || true
wait "$lease_pid" 2>/dev/null || true
lease_pid=
cargo xtask board ls >"$result_dir/board-list-after.log" 2>&1

if ((remote_status != 0)); then
    printf 'remote_status=%s\n' "$remote_status" >"$result_dir/failure-status.txt"
    write_checksums
    echo "RKNN Linux board execution failed; evidence preserved at $result_dir" >&2
    exit "$remote_status"
fi

finished_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
clean_source_argument=()
if ((require_clean == 1)); then
    clean_source_argument=(--require-clean-source)
fi
"$rknn_python" "$analyzer" analyze \
    --raw "$result_dir/board/raw.csv" \
    --console "$result_dir/board/console.log" \
    --corpus "$result_dir/board/corpus.csv" \
    --output "$result_dir/linux-reference-report.json" \
    --board-facts "$result_dir/board/board-facts.txt" \
    --ldd "$result_dir/board/ldd.log" \
    --deployed-runtime "$rknn_runtime" \
    --deployed-model "$result_dir/board/thermal-4x6x1-v1-rk3588-fp16.rknn" \
    --runner-binary "$result_dir/board/thermal_rknn_linux_reference" \
    --board-type "$board_type" \
    --remote-dir "$remote_dir" \
    --run-id "$run_id" \
    --source-commit "$source_commit" \
    --source-branch "$source_branch" \
    --source-dirty "$source_dirty" \
    --tracked-change-count "$tracked_change_count" \
    --untracked-file-count "$untracked_file_count" \
    --started-at "$started_at" \
    --finished-at "$finished_at" \
    "${clean_source_argument[@]}" \
    | tee "$result_dir/analyzer.log"

write_checksums
echo "THERMAL_RKNN_LINUX_BOARD_PASS result_dir=$result_dir run_id=$run_id"
