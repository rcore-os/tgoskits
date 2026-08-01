#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

usage() {
    cat <<'EOF'
usage: run.sh [idle|cpu-stress|all] [output-directory]

Build and run the native Zephyr v4.3.0 qemu_cortex_a53 real-time baseline.
The default output directory is tmp/competition/rt-baseline/zephyr.
EOF
}

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd -- "$script_dir/../../.." && pwd)
mode=${1:-all}
output_root=${2:-"$repo_root/tmp/competition/rt-baseline/zephyr"}
zephyr_base=${ZEPHYR_BASE:-"$repo_root/tmp/zephyr-v4.3.0"}
west_workspace=${WEST_WORKSPACE:-"$repo_root/tmp"}
west=${WEST:-"$repo_root/tmp/zephyr-venv/bin/west"}
cross_compile=${CROSS_COMPILE:-"$repo_root/tmp/zephyr-toolchain/bin/aarch64-zephyr-"}

if [[ "$output_root" != /* ]]; then
    output_root="$repo_root/$output_root"
fi

case "$mode" in
    idle|cpu-stress|all) ;;
    -h|--help)
        usage
        exit 0
        ;;
    *)
        usage >&2
        exit 2
        ;;
esac

for required in "$zephyr_base/VERSION" "$west_workspace/.west/config" "$west"; do
    if [[ ! -e "$required" ]]; then
        echo "missing Zephyr baseline dependency: $required" >&2
        exit 2
    fi
done
for command in git python3 qemu-system-aarch64 tee timeout \
    "${cross_compile}gcc"; do
    if ! command -v "$command" >/dev/null 2>&1; then
        echo "missing Zephyr baseline command: $command" >&2
        exit 2
    fi
done
if [[ $(git -C "$zephyr_base" describe --tags --exact-match) != v4.3.0 ]]; then
    echo "Zephyr source is not checked out at the exact v4.3.0 tag" >&2
    exit 2
fi

export ZEPHYR_BASE="$zephyr_base"
export ZEPHYR_TOOLCHAIN_VARIANT=cross-compile
export CROSS_COMPILE="$cross_compile"
source_provenance="$output_root/source-provenance.json"

if [[ "$mode" == all ]]; then
    workloads=(idle cpu-stress)
else
    workloads=("$mode")
fi

for workload in "${workloads[@]}"; do
    case_dir="$output_root/$workload"
    if [[ -e "$case_dir" ]]; then
        echo "refusing to overwrite native Zephyr evidence: $case_dir" >&2
        echo "choose a new output directory" >&2
        exit 2
    fi
done
if [[ -e "$source_provenance" ]]; then
    echo "refusing to overwrite native Zephyr evidence: $source_provenance" >&2
    exit 2
fi

run_case() {
    local workload=$1
    local case_dir="$output_root/$workload"
    local build_dir="$case_dir/build"
    local build_log="$case_dir/build.log"
    local raw_log="$case_dir/qemu.log"
    local -a build_command=(
        "$west" build -p always -b qemu_cortex_a53
        -d "$build_dir" "$script_dir"
    )

    mkdir -p -- "$case_dir"
    if [[ "$workload" == cpu-stress ]]; then
        build_command+=(-- "-DEXTRA_CONF_FILE=$script_dir/stress.conf")
    fi

    (
        cd -- "$west_workspace"
        "${build_command[@]}"
    ) 2>&1 | tee "$build_log"

    (
        cd -- "$west_workspace"
        timeout --foreground 60s "$west" build -d "$build_dir" -t run
    ) 2>&1 | tee "$raw_log"
}

analyze_case() {
    local workload=$1
    local case_dir="$output_root/$workload"
    local build_dir="$case_dir/build"
    local build_log="$case_dir/build.log"
    local raw_log="$case_dir/qemu.log"
    local summary="$case_dir/summary.json"

    python3 "$script_dir/analyze.py" "$raw_log" \
        --build-log "$build_log" \
        --build-dir "$build_dir" \
        --zephyr-base "$zephyr_base" \
        --source-provenance "$source_provenance" \
        --workload "$workload" \
        --output "$summary"
    echo "validated native Zephyr $workload result: $summary"
}

for workload in "${workloads[@]}"; do
    run_case "$workload"
done
python3 "$script_dir/capture_source.py" \
    --zephyr-base "$zephyr_base" \
    --output "$source_provenance"
for workload in "${workloads[@]}"; do
    analyze_case "$workload"
done
