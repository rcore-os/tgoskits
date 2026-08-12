#!/usr/bin/env bash
set -euo pipefail

workspace=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
images=$(cd "${TGOSIMAGES_DIR:-"${workspace}/../tgosimages"}" && pwd)
rootfs=${ROOTFS:-"${images}/IMAGES/rootfs/rootfs-aarch64-alpine.img"}

if [[ ! -f "${rootfs}" ]]; then
    echo "rootfs not found: ${rootfs}" >&2
    echo "Build it with: ${images}/build.sh platform qemu-aarch64 zephyr-net --rootfs alpine" >&2
    exit 1
fi

cd "${workspace}"
exec cargo xtask axvisor qemu \
    --config os/axvisor/configs/board/qemu-aarch64-zephyr-linux-virtio-net.toml \
    --qemu-config os/axvisor/configs/qemu/qemu-aarch64-zephyr-linux-virtio-net.toml \
    --rootfs "${rootfs}"
