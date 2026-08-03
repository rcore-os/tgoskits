#!/usr/bin/env bash

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
workspace=$(cd -- "$script_dir/../../.." && pwd)
toolchain=nightly-2026-07-15
output_dir=$workspace/tmp/competition/ivc/starry
starry_binary=$workspace/target/aarch64-unknown-linux-musl/release/starryos.bin

cd "$workspace"
cargo "+$toolchain" xtask starry build \
    -c competition/ivc/config/starry-aarch64.toml \
    --smp 2

mkdir -p "$output_dir"
install -m 0644 "$starry_binary" "$output_dir/starryos.bin"
"$script_dir/build-guest-dtb.sh"
"$script_dir/build-rootfs.sh" --profile smoke --policy neural --backend native \
    --output "$output_dir/starry-ivc-rootfs-smoke.img"
"$script_dir/build-rootfs.sh" --profile full --policy neural --backend native \
    --output "$output_dir/starry-ivc-rootfs.img"
"$script_dir/build-rootfs.sh" --profile smoke --policy manual --backend native \
    --output "$output_dir/starry-ivc-rootfs-manual-smoke.img"
"$script_dir/build-rootfs.sh" --profile full --policy manual --backend native \
    --output "$output_dir/starry-ivc-rootfs-manual.img"

sha256sum \
    "$output_dir/starryos.bin" \
    "$output_dir/starry-orangepi-5-plus.dtb" \
    "$output_dir/starry-ivc-rootfs-smoke.img" \
    "$output_dir/starry-ivc-rootfs.img" \
    "$output_dir/starry-ivc-rootfs-manual-smoke.img" \
    "$output_dir/starry-ivc-rootfs-manual.img"
echo "IVC StarryOS guest artifacts ready at $output_dir"
