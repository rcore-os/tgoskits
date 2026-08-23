#!/usr/bin/env bash
set -euo pipefail

workspace=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
cd "$workspace"

objcopy="${LLVM_OBJCOPY:-$(rustc --print sysroot)/lib/rustlib/x86_64-unknown-linux-gnu/bin/llvm-objcopy}"
if [[ ! -x "$objcopy" ]]; then
    echo "llvm-objcopy not found: $objcopy (set LLVM_OBJCOPY)" >&2
    exit 1
fi

cargo xtask arceos build \
    -p arceos-guest-ip-server \
    -c apps/arceos/build-aarch64-guest-ip-server.toml

raw="target/aarch64-unknown-none-softfloat/release/arceos-guest-ip-server"
bin="target/aarch64-unknown-none-softfloat/release/arceos-guest-ip-server.bin"
"$objcopy" --strip-all -O binary "$raw" "$bin"

exec cargo xtask axvisor qemu \
    --config os/axvisor/configs/board/qemu-aarch64-starry-rtos-gipc.toml \
    --qemu-config os/axvisor/configs/qemu/qemu-aarch64-starry-rtos-gipc.toml \
    --rootfs "${ROOTFS_IMAGE:?set ROOTFS_IMAGE to the Linux guest rootfs image}"
