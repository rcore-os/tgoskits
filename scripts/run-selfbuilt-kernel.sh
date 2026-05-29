#!/usr/bin/env bash
#
# run-selfbuilt-kernel.sh — Extract the self-compiled starryos from rootfs and
# boot it in QEMU.
#
# Prerequisites:
#   - scripts/self-compile.sh (must complete successfully first)
#   - qemu-system-<arch>, debugfs
#
# Usage:
#   ./scripts/run-selfbuilt-kernel.sh [OPTIONS] [rootfs-image]
#
#   --arch <arch>   Target architecture: riscv64 (default), x86_64, aarch64.
#                   Must match the arch used during self-compile.sh.
#   --smp <N>       Number of QEMU CPUs (default: 4).
#   rootfs-image    Path to the selfhost rootfs (by arch default).
#
#   x86_64 notes: KVM acceleration is enabled when /dev/kvm is available.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

# ─── Argument parsing ───────────────────────────────────────────────────────────

ARCH="riscv64"
SMP=4
ROOTFS_IMG=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --arch) ARCH="$2"; shift 2 ;;
        --smp)  SMP="$2"; shift 2 ;;
        --help|-h)
            echo "Usage: $0 [--arch riscv64|x86_64|aarch64] [--smp N] [rootfs-image]"
            exit 0
            ;;
        *) ROOTFS_IMG="$1"; shift ;;
    esac
done

# ─── Architecture mapping ───────────────────────────────────────────────────────

case "$ARCH" in
    riscv64)
        TARGET="riscv64gc-unknown-none-elf"
        QEMU_BIN="qemu-system-riscv64"
        QEMU_MACHINE="virt"
        QEMU_CPU="rv64"
        QEMU_EXTRA=""
        QEMU_BLK_DEV="virtio-blk-pci,drive=disk0"
        ;;
    x86_64)
        TARGET="x86_64-unknown-none"
        QEMU_BIN="qemu-system-x86_64"
        QEMU_MACHINE="q35"
        if [ -r /dev/kvm ] && [ -w /dev/kvm ]; then
            QEMU_CPU="host"
            QEMU_EXTRA="-enable-kvm"
        else
            QEMU_CPU="IvyBridge"
            QEMU_EXTRA=""
        fi
        QEMU_BLK_DEV="virtio-blk-pci,drive=disk0"
        ;;
    aarch64)
        TARGET="aarch64-unknown-none-softfloat"
        QEMU_BIN="qemu-system-aarch64"
        QEMU_MACHINE="virt"
        QEMU_CPU="cortex-a72"
        QEMU_EXTRA=""
        QEMU_BLK_DEV="virtio-blk-device,drive=disk0"
        ;;
    *)
        printf "[run-selfbuilt] ERROR: Unsupported arch: %s (valid: riscv64, x86_64, aarch64)\n" "$ARCH" >&2
        exit 1
        ;;
esac

# Default rootfs image per arch
if [ -z "$ROOTFS_IMG" ]; then
    case "$ARCH" in
        riscv64)  ROOTFS_IMG="tmp/axbuild/rootfs/rootfs-riscv64-debian-selfhost-v2.img" ;;
        x86_64)   ROOTFS_IMG="tmp/axbuild/rootfs/rootfs-x86_64-debian-selfhost.img" ;;
        aarch64)  ROOTFS_IMG="tmp/axbuild/rootfs/rootfs-aarch64-debian-selfhost.img" ;;
    esac
fi

CACHED_KERNEL="$REPO_ROOT/tmp/starryos-selfbuilt-${ARCH}"

info()  { printf "[run-selfbuilt] %s\n" "$*"; }
error() { printf "[run-selfbuilt] ERROR: %s\n" "$*" >&2; exit 1; }

info "Architecture: $ARCH | QEMU: $QEMU_BIN | SMP: $SMP"

# ─── Prerequisite checks ───────────────────────────────────────────────────────

for cmd in "$QEMU_BIN" debugfs; do
    command -v "$cmd" &>/dev/null || error "$cmd not found"
done

[ -f "$ROOTFS_IMG" ] || error "Rootfs image not found: $ROOTFS_IMG"

# ─── Step 1: Extract the self-compiled kernel ──────────────────────────────────

if [ -f "$CACHED_KERNEL" ] && [ -s "$CACHED_KERNEL" ]; then
    EXISTING_SIZE=$(stat -c%s "$CACHED_KERNEL" 2>/dev/null || echo "0")
    info "Using cached kernel: $CACHED_KERNEL (${EXISTING_SIZE} bytes)"
    info "Delete this file to force re-extraction from rootfs."
else
    info "Extracting self-compiled kernel from rootfs..."
    mkdir -p "$(dirname "$CACHED_KERNEL")"

    debugfs -R "dump /opt/starryos-selfbuilt $CACHED_KERNEL" "$ROOTFS_IMG" 2>/dev/null || true

    if [ ! -f "$CACHED_KERNEL" ] || [ ! -s "$CACHED_KERNEL" ]; then
        error "Failed to extract kernel from rootfs. Did you run scripts/self-compile.sh first?"
    fi

    KERNEL_SIZE=$(stat -c%s "$CACHED_KERNEL")
    info "Kernel extracted: $CACHED_KERNEL (${KERNEL_SIZE} bytes)"
fi

# ─── Step 2: Show kernel info ─────────────────────────────────────────────────

info "Kernel path: $CACHED_KERNEL"
info "Kernel size: $(stat -c%s "$CACHED_KERNEL") bytes"
info "Kernel md5:  $(md5sum "$CACHED_KERNEL" | cut -d' ' -f1)"

# ─── Step 3: Boot ─────────────────────────────────────────────────────────────

info "Booting self-compiled StarryOS kernel ($ARCH)..."
info "Press Ctrl+A then X to exit QEMU."

exec "$QEMU_BIN" \
    -nographic \
    -machine "$QEMU_MACHINE" \
    -cpu "$QEMU_CPU" \
    $QEMU_EXTRA \
    -smp "$SMP" \
    -m 8G \
    -kernel "$CACHED_KERNEL" \
    -device "$QEMU_BLK_DEV" \
    -drive id=disk0,if=none,format=raw,file="$ROOTFS_IMG",file.locking=off \
    -device virtio-net-pci,netdev=net0 \
    -netdev user,id=net0
