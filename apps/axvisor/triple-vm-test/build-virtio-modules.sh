#!/usr/bin/env bash
# Use the prepared source/build tree of the exact Linux guest kernel.
set -euo pipefail
if [[ $# -ne 2 ]]; then
    echo 'Usage: ./build-virtio-modules.sh KERNEL_BUILD_TREE OUTPUT_DIRECTORY' >&2
    exit 2
fi
kernel=$(cd -- "$1" && pwd -P)
[[ -s $kernel/Module.symvers && -s $kernel/include/generated/utsrelease.h ]]
mkdir -p -- "$2"
output=$(cd -- "$2" && pwd -P)
cp "$kernel/drivers/virtio/virtio_mmio.c" "$output/"
cp "$kernel/drivers/block/virtio_blk.c" "$output/"
printf 'obj-m += virtio_mmio.o virtio_blk.o\n' > "$output/Makefile"
make -C "$kernel" ARCH=arm64 CROSS_COMPILE="${CROSS_COMPILE:-aarch64-linux-gnu-}" \
    M="$output" -j"${JOBS:-4}" modules
modinfo -F vermagic "$output/virtio_mmio.ko"
modinfo -F vermagic "$output/virtio_blk.ko"
