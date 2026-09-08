#!/bin/bash
# Checks the QEMU log of the hello-efi integration test for the UEFI
# application output printed through ArceBoot's ConOut protocol.
set -e

LOG_FILE="${1:-qemu-hello-efi.log}"
TARGET_STRING="EFI Output: Hello from a riscv64 UEFI app running on ArceBoot!"

if [ ! -f "$LOG_FILE" ]; then
    echo "FAIL: $LOG_FILE does not exist"
    exit 1
fi

if grep -qF "$TARGET_STRING" "$LOG_FILE"; then
    echo "PASS: found the UEFI app output in $LOG_FILE"
    exit 0
else
    echo "FAIL: UEFI app output not found in $LOG_FILE"
    exit 2
fi
