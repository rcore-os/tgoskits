#!/bin/bash
# Build a minimal riscv64 UEFI application and pack it into a ramdisk for
# ArceBoot integration tests.
#
# The riscv64gc-unknown-uefi rustc target was removed upstream (LLVM has no
# riscv COFF object support) and GNU ld's pei-riscv64 emulation is broken, so
# the flow mirrors EDK2's GenFw: build a static PIC ELF with the GNU riscv64
# toolchain, then convert it to a PE32+ image with elf2pe.py.
#
# Outputs: tests/hello-efi/BOOTRISCV64.EFI and ramdisk.cpio (at the repo root
# of the working directory).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HELLO_DIR="$SCRIPT_DIR/../../tests/hello-efi"

PREFIX="${RISCV64_ELF_PREFIX:-riscv64-unknown-elf-}"

cd "$HELLO_DIR"

"${PREFIX}gcc" -c -fPIC -ffreestanding -mcmodel=medany -march=rv64gc -mabi=lp64d \
    -o hello.o hello.c
"${PREFIX}ld" -m elf64lriscv -e efi_main -o hello.elf hello.o
# No relocations must remain: the PE is loaded at an arbitrary address by
# ArceBoot and relies on pure PC-relative code.
if "${PREFIX}readelf" -r hello.elf | grep -q R_RISCV; then
    echo "error: hello.elf has relocations; PIC build is broken" >&2
    exit 1
fi

python3 elf2pe.py hello.elf BOOTRISCV64.EFI

# Pack the ramdisk: entries must not carry a "./" prefix (ArceBoot matches
# bare names), and the test file is read by ArceBoot's ramdisk check.
RAMDISK_DIR="$HELLO_DIR/ramdisk"
rm -rf "$RAMDISK_DIR"
mkdir -p "$RAMDISK_DIR/EFI/BOOT" "$RAMDISK_DIR/test"
cp BOOTRISCV64.EFI "$RAMDISK_DIR/EFI/BOOT/"
echo "This is a test file for ArceBoot." > "$RAMDISK_DIR/test/arceboot.txt"
(
    cd "$RAMDISK_DIR"
    {
        echo test
        echo test/arceboot.txt
        echo EFI
        echo EFI/BOOT
        echo EFI/BOOT/BOOTRISCV64.EFI
    } | cpio -o --format=newc
) > "$HELLO_DIR/ramdisk.cpio"

echo "Built $HELLO_DIR/BOOTRISCV64.EFI and $HELLO_DIR/ramdisk.cpio"
