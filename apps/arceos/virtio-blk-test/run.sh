#!/usr/bin/env bash
set -euo pipefail

workspace=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
cd "$workspace"

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

exec cargo xtask axvisor qemu \
    --config os/axvisor/configs/board/qemu-aarch64-virtio-blk-test.toml \
    --qemu-config os/axvisor/configs/qemu/qemu-aarch64-virtio-blk-test.toml
