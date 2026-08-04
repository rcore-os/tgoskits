#!/usr/bin/env bash

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
workspace=$(cd -- "$script_dir/../../.." && pwd)
output_dir=$workspace/tmp/competition/ivc/starry
smoke_rootfs=$output_dir/starry-ivc-rootfs-rknpu-smoke.img
full_rootfs=$output_dir/starry-ivc-rootfs-rknpu.img

bash "$script_dir/build-rknpu-offline.sh"
bash "$script_dir/build-rknpu-control-rootfs.sh" \
    --profile smoke \
    --output "$smoke_rootfs"
bash "$script_dir/build-rknpu-control-rootfs.sh" \
    --profile full \
    --output "$full_rootfs"

sha256sum \
    "$output_dir/starryos-rknpu.bin" \
    "$output_dir/starry-orangepi-5-plus-rknpu.dtb" \
    "$smoke_rootfs" \
    "$full_rootfs"
echo "STARRY_RKNN_CONTROL_ARTIFACTS_PASS output_dir=$output_dir"
