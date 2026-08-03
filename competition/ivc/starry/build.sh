#!/usr/bin/env bash

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
workspace=$(cd -- "$script_dir/../../.." && pwd)
toolchain=nightly-2026-07-15
output_dir=$workspace/tmp/competition/ivc/starry
built_elf=$workspace/target/aarch64-unknown-linux-musl/release/starryos
built_kernel=$workspace/target/aarch64-unknown-linux-musl/release/starryos.bin

for command_name in cargo install rustup sha256sum; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
        echo "Required StarryOS build command not found: $command_name" >&2
        exit 1
    fi
done

cd "$workspace"
cargo "+$toolchain" xtask starry build \
    -c competition/ivc/config/starry-aarch64.toml \
    --smp 2
if [[ ! -s "$built_elf" ]]; then
    echo "StarryOS build did not produce $built_elf" >&2
    exit 1
fi
rustup run "$toolchain" llvm-objcopy --strip-all -O binary \
    "$built_elf" "$built_kernel"
if [[ ! -s "$built_kernel" ]]; then
    echo "StarryOS build did not produce $built_kernel" >&2
    exit 1
fi

mkdir -p "$output_dir"
install -m 0644 "$built_kernel" "$output_dir/starryos.bin"
"$script_dir/build-guest-dtb.sh"
"$script_dir/build-rootfs.sh" --profile smoke --policy neural --backend native \
    --output "$output_dir/starry-ivc-rootfs-smoke.img"
"$script_dir/build-rootfs.sh" --profile full --policy neural --backend native \
    --output "$output_dir/starry-ivc-rootfs.img"
"$script_dir/build-rootfs.sh" --profile smoke --policy manual --backend native \
    --output "$output_dir/starry-ivc-rootfs-manual-smoke.img"
"$script_dir/build-rootfs.sh" --profile full --policy manual --backend native \
    --output "$output_dir/starry-ivc-rootfs-manual.img"
"$script_dir/build-rootfs.sh" --profile full --policy neural --backend native \
    --count 100 --output "$output_dir/starry-ivc-rootfs-ack-loss.img"

sha256sum \
    "$output_dir/starryos.bin" \
    "$output_dir/starry-orangepi-5-plus.dtb" \
    "$output_dir/starry-ivc-rootfs-smoke.img" \
    "$output_dir/starry-ivc-rootfs.img" \
    "$output_dir/starry-ivc-rootfs-manual-smoke.img" \
    "$output_dir/starry-ivc-rootfs-manual.img" \
    "$output_dir/starry-ivc-rootfs-ack-loss.img"
echo "IVC StarryOS guest artifacts ready at $output_dir"
