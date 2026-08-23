#!/usr/bin/env bash
set -euo pipefail

workspace=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
cd "$workspace"

rootfs="${ROOTFS_IMAGE:?set ROOTFS_IMAGE to the Linux guest rootfs image}"

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

linux_client="${GIPC_LINUX_CLIENT_BIN:-target/gipc-linux-client}"
if [[ ! -x "$linux_client" ]]; then
    cc -std=c11 -Wall -Wextra -O2 \
        apps/starry/guest-ip-link/linux-client.c -o "$linux_client"
fi
if [[ "${GIPC_INJECT_CLIENT:-1}" == "1" ]]; then
    command -v debugfs >/dev/null || {
        echo "debugfs is required to inject the Linux client into $rootfs" >&2
        exit 1
    }
    debugfs -w -R "rm /usr/bin/gipc-linux-client" "$rootfs" >/dev/null 2>&1 || true
    debugfs -w -R "write $linux_client /usr/bin/gipc-linux-client" "$rootfs" >/dev/null
    debugfs -w -R "mkdir /etc/profile.d" "$rootfs" >/dev/null 2>&1 || true
    debugfs -w -R "rm /etc/profile.d/99-gipc.sh" "$rootfs" >/dev/null 2>&1 || true
    debugfs -w -R "write apps/starry/guest-ip-link/gipc-autostart.sh /etc/profile.d/99-gipc.sh" \
        "$rootfs" >/dev/null
fi

exec cargo xtask axvisor qemu \
    --config os/axvisor/configs/board/qemu-aarch64-starry-rtos-gipc.toml \
    --qemu-config os/axvisor/configs/qemu/qemu-aarch64-starry-rtos-gipc.toml \
    --rootfs "$rootfs"
