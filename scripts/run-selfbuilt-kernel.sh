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
#   --kernel <path> Self-compiled kernel binary (default: tmp/starryos-selfbuilt-<arch>).
#                   Rootfs defaults to the arch-specific image unless specified.
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
KERNEL_PATH=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --arch)   ARCH="$2"; shift 2 ;;
        --smp)    SMP="$2"; shift 2 ;;
        --kernel) KERNEL_PATH="$2"; shift 2 ;;
        --help|-h)
            echo "Usage: $0 [--arch riscv64|x86_64|aarch64] [--smp N] [--kernel <path>] [rootfs-image]"
            echo "  --kernel <path>  Self-compiled kernel binary (default: tmp/starryos-selfbuilt-<arch>)"
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
        QEMU_NET_DEV="virtio-net-pci,netdev=net0"
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
        QEMU_NET_DEV="virtio-net-pci,netdev=net0"
        ;;
    aarch64)
        TARGET="aarch64-unknown-none-softfloat"
        QEMU_BIN="qemu-system-aarch64"
        QEMU_MACHINE="virt"
        QEMU_CPU="cortex-a72"
        QEMU_EXTRA=""
        QEMU_BLK_DEV="virtio-blk-device,drive=disk0"
        QEMU_NET_DEV="virtio-net-device,netdev=net0"
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
        x86_64)   ROOTFS_IMG="tmp/debian-selfhost/rootfs-x86_64-debian-selfhost.img" ;;
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

# ─── Step 1: Resolve the self-compiled kernel ──────────────────────────────────

if [ -n "$KERNEL_PATH" ]; then
    # Explicit kernel path — use directly, no extraction needed.
    if [ ! -f "$KERNEL_PATH" ] || [ ! -s "$KERNEL_PATH" ]; then
        error "Specified kernel not found or empty: $KERNEL_PATH"
    fi
    CACHED_KERNEL="$KERNEL_PATH"
    info "Using specified kernel: $CACHED_KERNEL"
elif [ -f "$CACHED_KERNEL" ] && [ -s "$CACHED_KERNEL" ]; then
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

if [ -f "$CACHED_KERNEL" ] && [ -s "$CACHED_KERNEL" ]; then
    info "Kernel path: $CACHED_KERNEL"
    info "Kernel size: $(stat -c%s "$CACHED_KERNEL") bytes"
    info "Kernel md5:  $(md5sum "$CACHED_KERNEL" | cut -d' ' -f1)"
fi

# ─── Step 3: Boot ─────────────────────────────────────────────────────────────
#
# x86_64: the self-compiled kernel IS an EFI binary (built with plat-dyn
# + axplat-dyn/efi).  We convert it to raw binary (objcopy), place it in
# an EFI System Partition, and boot via OVMF UEFI firmware — the same
# boot path used by the seed kernel.
#
# riscv64 / aarch64: bare-metal ELF booted via QEMU's -kernel loader.

case "$ARCH" in
    x86_64)
        info "Booting self-compiled kernel via UEFI / OVMF ..."

        [ -f "$CACHED_KERNEL" ] || error "Self-compiled kernel not found: $CACHED_KERNEL (run scripts/self-compile.sh first)"

        # The self-compiled kernel is an x86_64-unknown-none ELF whose
        # EFI stub (axplat-dyn/efi) makes it an EFI application.  Strip
        # the ELF headers (objcopy -O binary) to produce the raw PE/COFF
        # payload that OVMF loads from the EFI System Partition.
        command -v objcopy &>/dev/null || error "objcopy not found (install binutils)"

        BIN_KERNEL="$REPO_ROOT/tmp/starryos-selfbuilt-${ARCH}.bin"
        info "Converting ELF to raw binary for UEFI boot..."
        objcopy -O binary "$CACHED_KERNEL" "$BIN_KERNEL" || error "objcopy failed"
        info "Raw binary: $(stat -c%s "$BIN_KERNEL") bytes"

        ESP_DIR="$REPO_ROOT/tmp/esp-${ARCH}"
        rm -rf "$ESP_DIR"
        mkdir -p "$ESP_DIR/EFI/BOOT"
        cp "$BIN_KERNEL" "$ESP_DIR/EFI/BOOT/BOOTX64.EFI"
        info "ESP ready: $ESP_DIR/EFI/BOOT/BOOTX64.EFI"

        # OVMF firmware — search common distribution paths.
        # Arch:    /usr/share/edk2/x64/OVMF_CODE.4m.fd  (edk2-ovmf)
        # Debian:  /usr/share/OVMF/OVMF_CODE_4M.fd      (ovmf)
        # Fedora:  /usr/share/edk2/ovmf/OVMF_CODE.fd    (edk2-ovmf)
        # Generic: /usr/share/ovmf/OVMF.fd, /usr/share/qemu/OVMF.fd
        OVMF_CODE=""
        for candidate in \
            /usr/share/edk2/x64/OVMF_CODE.4m.fd \
            /usr/share/OVMF/OVMF_CODE_4M.fd \
            /usr/share/OVMF/OVMF_CODE.fd \
            /usr/share/edk2/ovmf/OVMF_CODE.fd \
            /usr/share/ovmf/OVMF.fd \
            /usr/share/qemu/OVMF.fd; do
            if [ -f "$candidate" ]; then
                OVMF_CODE="$candidate"
                break
            fi
        done
        [ -n "$OVMF_CODE" ] || error "OVMF firmware not found; install edk2-ovmf or ovmf"
        info "OVMF firmware: $OVMF_CODE"

        # Derive VARS template from same directory as CODE.
        OVMF_DIR="$(dirname "$OVMF_CODE")"
        OVMF_VARS_TEMPLATE=""
        for candidate in \
            "${OVMF_DIR}/OVMF_VARS.4m.fd" \
            "${OVMF_DIR}/OVMF_VARS_4M.fd" \
            "${OVMF_DIR}/OVMF_VARS.fd"; do
            if [ -f "$candidate" ]; then
                OVMF_VARS_TEMPLATE="$candidate"
                break
            fi
        done
        [ -n "$OVMF_VARS_TEMPLATE" ] || error "OVMF_VARS not found alongside OVMF_CODE in $OVMF_DIR"
        OVMF_VARS="$REPO_ROOT/tmp/OVMF_VARS.x86_64.fd"
        if [ ! -f "$OVMF_VARS" ]; then
            cp "$OVMF_VARS_TEMPLATE" "$OVMF_VARS"
        fi

        info "Boot via OVMF UEFI: self-compiled kernel as EFI/BOOT/BOOTX64.EFI"
        info "Press Ctrl+A then X to exit QEMU."

        exec "$QEMU_BIN" \
            -nographic \
            -machine "$QEMU_MACHINE" \
            -cpu "$QEMU_CPU" \
            $QEMU_EXTRA \
            -smp "$SMP" \
            -m 8G \
            -drive if=pflash,format=raw,unit=0,readonly=on,file="$OVMF_CODE" \
            -drive if=pflash,format=raw,unit=1,file="$OVMF_VARS" \
            -drive format=raw,file=fat:rw:"$ESP_DIR" \
            -device virtio-blk-pci,drive=disk0 \
            -drive id=disk0,if=none,format=raw,file="$ROOTFS_IMG",file.locking=off \
            -device virtio-net-pci,netdev=net0 \
            -netdev user,id=net0
        ;;
    *)
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
            -device "$QEMU_NET_DEV" \
            -netdev user,id=net0
        ;;
esac
