#!/usr/bin/env bash

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
workspace=$(cd -- "$script_dir/../../../.." && pwd)
output_dir=$workspace/tmp/axvisor-rt/guest-dtb
output=$output_dir/starry-orangepi-5-plus.dtb

mkdir -p "$output_dir"
dtc -I dts -O dtb -o "$output" "$script_dir/orangepi-5-plus.dts"
dtc -I dtb -O dts -o /dev/null "$output"
if fdtget -l "$output" / | grep -q '^[[:space:]]*virtio_mmio@'; then
    echo "Guest DTB template must not predeclare graph-owned virtio-mmio nodes" >&2
    exit 1
fi
sha256sum "$output"
echo "IVC StarryOS guest DTB ready at $output"
