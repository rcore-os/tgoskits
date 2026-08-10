#!/usr/bin/env bash
set -euo pipefail

workspace=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
cd "$workspace"

objcopy="${LLVM_OBJCOPY:-}"
if [[ -z "$objcopy" ]]; then
    objcopy="$(rustc --print sysroot)/lib/rustlib/x86_64-unknown-linux-gnu/bin/llvm-objcopy"
fi
if [[ ! -x "$objcopy" ]]; then
    echo "llvm-objcopy not found: $objcopy (set LLVM_OBJCOPY)" >&2
    exit 1
fi

build_guest() {
    local vm="$1"
    local config="apps/arceos/build-aarch64-virtio-net-peer-${vm}.toml"
    local output="target/aarch64-unknown-linux-musl/release/arceos-virtio-net-peer-${vm}.bin"

    cargo xtask arceos build -p arceos-virtio-net-peer -c "$config"
    "$objcopy" --strip-all -O binary \
        target/aarch64-unknown-linux-musl/release/arceos-virtio-net-peer \
        "$output"
}

build_guest vm1
build_guest vm2

exec cargo xtask axvisor qemu \
    --config os/axvisor/configs/board/qemu-aarch64-virtio-net-peer.toml \
    --qemu-config os/axvisor/configs/qemu/qemu-aarch64-virtio-net-peer.toml
