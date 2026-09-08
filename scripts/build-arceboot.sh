#!/usr/bin/env bash
# Build ArceBoot, the cross-platform bootloader migrated from rustsbi.
#
# This script mirrors the arceboot build flow that lived in the rustsbi
# xtask: cargo build with the arceboot linker script, then ELF -> raw binary.
#
# Configuration knobs (all optional):
#   AX_FEATURES   extra cargo features (e.g. "net,display")
#   AX_LOG        log level passed as AX_LOG env (default: debug)
#   AX_PROFILE    cargo profile (default: release)
#
# Ramdisk boot config (see bootloader/arceboot/src/config.rs):
#   AX_USE_RAMDISK=1      enable ramdisk support
#   AX_RAMDISK_START      physical start of the pre-loaded ramdisk (hex)
#   AX_RAMDISK_SIZE       ramdisk size in bytes (hex)
#
# Usage:
#   scripts/build-arceboot.sh [extra cargo args...]
#   AX_FEATURES="net,display" scripts/build-arceboot.sh

set -euo pipefail

cd "$(dirname "$0")/.."

TARGET=riscv64gc-unknown-none-elf
FEATURES=${AX_FEATURES:-}
PROFILE=${AX_PROFILE:-release}

# The linker script is wired through bootloader/arceboot/build.rs
# (`-Taxplat.x`, the axplat-dyn -> somehal -> someboot script chain; see
# scripts/axbuild/src/build/info.rs). The riscv64 load address defaults to
# 0x8020_0000 (override with SOMEBOOT_RISCV64_KERNEL_LOAD_PADDR). The
# upstream arceboot link.ld is kept for reference but is not used by this
# build.
RUSTFLAGS="-Aunused-features -C opt-level=z -C panic=abort -C relocation-model=pic" \
AX_LOG=${AX_LOG:-debug} \
cargo build \
    --package arceboot \
    --target "$TARGET" \
    --profile "$PROFILE" \
    ${FEATURES:+--features "$FEATURES"} \
    "$@"

if command -v rust-objcopy >/dev/null 2>&1; then
    OBJCOPY="rust-objcopy --binary-architecture=riscv64"
elif command -v llvm-objcopy >/dev/null 2>&1; then
    # The pinned toolchain ships llvm-tools; llvm-objcopy needs no explicit
    # architecture for ELF-to-binary conversion.
    OBJCOPY=llvm-objcopy
else
    echo "error: neither rust-objcopy nor llvm-objcopy found" >&2
    exit 1
fi

$OBJCOPY -O binary \
    "target/$TARGET/$PROFILE/arceboot" \
    "target/$TARGET/$PROFILE/arceboot.bin"

echo "ArceBoot binary: target/$TARGET/$PROFILE/arceboot.bin"
