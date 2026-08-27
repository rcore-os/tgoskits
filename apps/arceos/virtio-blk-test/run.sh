#!/usr/bin/env bash
set -euo pipefail

workspace=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
cd "$workspace"

temp_parent="$workspace/tmp/axbuild"
temp_prefix="$temp_parent/virtio-blk-rootfs."
temp_dir=""

cleanup() {
    local status=$?
    trap - EXIT INT TERM
    if [[ -n "$temp_dir" && "$temp_dir" == "$temp_prefix"* && "$temp_dir" != "$temp_prefix" && -d "$temp_dir" ]]; then
        rm -rf -- "$temp_dir"
    fi
    exit "$status"
}
trap cleanup EXIT INT TERM

objcopy="${LLVM_OBJCOPY:-$(rustc --print sysroot)/lib/rustlib/x86_64-unknown-linux-gnu/bin/llvm-objcopy}"
if [[ ! -x "$objcopy" ]]; then
    echo "llvm-objcopy not found: $objcopy (set LLVM_OBJCOPY)" >&2
    exit 1
fi

cargo xtask arceos build -p arceos-virtio-blk-test \
    -c apps/arceos/build-aarch64-virtio-blk-test.toml
"$objcopy" --strip-all -O binary \
    target/aarch64-unknown-linux-musl/release/arceos-virtio-blk-test \
    target/aarch64-unknown-linux-musl/release/arceos-virtio-blk-test.bin

mkdir -p "$temp_parent"
temp_dir=$(mktemp -d "$temp_prefix"XXXXXX)
cargo xtask image pull rootfs-aarch64-alpine.img --extract-dir "$temp_dir"
temp_rootfs="$temp_dir/rootfs-aarch64-alpine.img"
if [[ ! -f "$temp_rootfs" ]]; then
    echo "extracted rootfs not found: $temp_rootfs" >&2
    exit 1
fi

cargo xtask axvisor qemu \
    --config os/axvisor/configs/board/qemu-aarch64-virtio-blk-test.toml \
    --qemu-config os/axvisor/configs/qemu/qemu-aarch64-virtio-blk-test.toml \
    --rootfs "$temp_rootfs"
