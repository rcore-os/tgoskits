#!/usr/bin/env bash

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
workspace=$(cd -- "$script_dir/../../.." && pwd)
toolchain=nightly-2026-07-15
output_dir=$workspace/tmp/competition/ivc/starry
built_elf=$workspace/target/aarch64-unknown-linux-musl/release/starryos
built_kernel=$workspace/target/aarch64-unknown-linux-musl/release/starryos.bin
kernel=$output_dir/starryos-rknpu.bin
guest_dtb=$output_dir/starry-orangepi-5-plus-rknpu.dtb
rootfs=$output_dir/starry-rknpu-rootfs-smoke.img

for command_name in cargo dtc rustup sha256sum; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
        echo "Required StarryOS RKNPU artifact command not found: $command_name" >&2
        exit 1
    fi
done

cd "$workspace"
cargo "+$toolchain" xtask starry build \
    -c competition/ivc/config/starry-aarch64-rknpu.toml \
    --smp 2
if [[ ! -s "$built_elf" ]]; then
    echo "StarryOS RKNPU build did not produce $built_elf" >&2
    exit 1
fi
rustup run "$toolchain" llvm-objcopy --strip-all -O binary \
    "$built_elf" "$built_kernel"
if [[ ! -s "$built_kernel" ]]; then
    echo "StarryOS RKNPU objcopy did not produce $built_kernel" >&2
    exit 1
fi

mkdir -p "$output_dir"
install -m 0644 "$built_kernel" "$kernel"
dtc -I dts -O dtb -o "$guest_dtb" \
    "$script_dir/orangepi-5-plus-rknpu.dts"
dtc -I dtb -O dts -o /dev/null "$guest_dtb"
bash "$script_dir/build-rknpu-rootfs.sh" "$rootfs"

sha256sum "$kernel" "$guest_dtb" "$rootfs"
echo "STARRY_RKNN_ARTIFACTS_PASS output_dir=$output_dir"
