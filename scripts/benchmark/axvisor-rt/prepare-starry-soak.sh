#!/usr/bin/env bash

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
workspace=$(git -C "$script_dir" rev-parse --show-toplevel)
kernel_builder=$script_dir/build-starry-kernel.sh
rootfs_builder=$script_dir/build-starry-rootfs.sh
kernel_config=$script_dir/config/starry-aarch64-rt-soak.toml
kernel_output=$workspace/tmp/axvisor-rt/starryos-rt-soak.bin
rootfs_output=$workspace/tmp/axvisor-rt/starry-rt-soak-rootfs.img

iterations=10000
warmup=100
period_us=90000
minimum_duration_seconds=1800
timed_metric_count=2

nominal_duration_seconds=$((iterations * period_us * timed_metric_count / 1000000))
if ((nominal_duration_seconds < minimum_duration_seconds)); then
    echo "soak timed phases cover only ${nominal_duration_seconds}s" >&2
    exit 1
fi

STARRY_RT_CONFIG=$kernel_config \
STARRY_RT_KERNEL_OUTPUT=$kernel_output \
    "$kernel_builder"

"$rootfs_builder" \
    --mode capture \
    --workload idle \
    --iterations "$iterations" \
    --warmup "$warmup" \
    --period-us "$period_us" \
    --measurement-cpu 0 \
    --stress-cpu 1 \
    --fifo-priority 80 \
    --output "$rootfs_output"

sha256sum "$kernel_config" "$kernel_output" "$rootfs_output"
echo "AXVISOR_RT_STARRY_SOAK_READY kernel=$kernel_output rootfs=$rootfs_output iterations=$iterations period_us=$period_us nominal_duration_seconds=$nominal_duration_seconds"
