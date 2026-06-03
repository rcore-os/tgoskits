#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
if ! WORKTREE_ROOT=$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel 2>/dev/null); then
    WORKTREE_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../../../../../../.." && pwd)
fi

TARGET_DIR=${STARRY_TRACE_TARGET_DIR:-"$WORKTREE_ROOT/target/k230-nncase-runtime"}
TRACE_FILE=${STARRY_TRACE_FILE:-"$TARGET_DIR/starry-nncase-kpu-trace.log"}
UART_LOG=${STARRY_TRACE_UART_LOG:-"$TARGET_DIR/starry-nncase-uart.log"}
BOOT_WAIT=${STARRY_TRACE_BOOT_WAIT:-4}
RUN_WAIT=${STARRY_TRACE_RUN_WAIT:-120}
TIMEOUT_SEC=${STARRY_TRACE_TIMEOUT:-180}
GUEST_CMD=${STARRY_TRACE_GUEST_CMD:-/usr/bin/k230-nncase-runtime-demo}

KERNEL="$WORKTREE_ROOT/target/riscv64gc-unknown-none-elf/release/starryos.bin"
DTB="$WORKTREE_ROOT/os/StarryOS/configs/board/k230-canmv.dtb"
PCBIOS="$WORKTREE_ROOT/target/qemu-k230-docker-build/pc-bios"
CASE_QEMU_DIR="$WORKTREE_ROOT/target/riscv64gc-unknown-none-elf/qemu-cases/qemu-k230/kpu-nncase-runtime"
ROOTFS=${STARRY_TRACE_ROOTFS:-}
QEMU=${QEMU_SYSTEM_RISCV64:-"$WORKTREE_ROOT/target/qemu-k230-docker-build/qemu-system-riscv64"}

if [ -z "$ROOTFS" ]; then
    mapfile -t ROOTFS_CANDIDATES < <(
        find "$CASE_QEMU_DIR" -type f \( -name case-rootfs.img -o -path '*/cache/rootfs/*.img' \) -print 2>/dev/null
    )
    if [ "${#ROOTFS_CANDIDATES[@]}" -gt 0 ]; then
        ROOTFS=$(ls -t "${ROOTFS_CANDIDATES[@]}" | head -n 1)
    fi
fi

if [ ! -x "$QEMU" ]; then
    QEMU=$(command -v qemu-system-riscv64)
fi

if [ ! -e "$KERNEL" ]; then
    echo "trace-nncase-runtime: missing kernel: $KERNEL" >&2
    echo "Run: cargo xtask starry test qemu --test-group k230-qemu --arch riscv64 -c kpu-nncase-runtime" >&2
    exit 1
fi
if [ ! -e "$ROOTFS" ]; then
    echo "trace-nncase-runtime: missing case rootfs under $CASE_QEMU_DIR" >&2
    echo "Run: cargo xtask starry test qemu --test-group k230-qemu --arch riscv64 -c kpu-nncase-runtime" >&2
    exit 1
fi

mkdir -p "$TARGET_DIR"
rm -f "$TRACE_FILE" "$UART_LOG"

echo "trace-nncase-runtime: qemu=$QEMU"
echo "trace-nncase-runtime: rootfs=$ROOTFS"
echo "trace-nncase-runtime: trace=$TRACE_FILE"
echo "trace-nncase-runtime: uart=$UART_LOG"

(
    sleep "$BOOT_WAIT"
    printf '%s\n' "$GUEST_CMD"
    sleep "$RUN_WAIT"
    printf '\001x'
) | timeout --foreground "$TIMEOUT_SEC" "$QEMU" \
    -L "$PCBIOS" \
    -machine k230 \
    -smp 2 \
    -m 2G \
    -nographic \
    -dtb "$DTB" \
    -drive "if=sd,format=raw,file=$ROOTFS" \
    -snapshot \
    -kernel "$KERNEL" \
    -trace enable=k230_kpu_start \
    -trace enable=k230_kpu_gnne_summary \
    -trace enable=k230_kpu_gnne_compute_summary \
    -trace enable=k230_kpu_runtime_arg_table \
    -trace enable=k230_kpu_l2_load \
    -trace enable=k230_kpu_l2_load_detail \
    -trace enable=k230_kpu_l2_load_hash \
    -trace enable=k230_kpu_l2_load_w \
    -trace enable=k230_kpu_l2_store \
    -trace enable=k230_kpu_l2_store_detail \
    -trace enable=k230_kpu_l2_store_hash \
    -trace "file=$TRACE_FILE" \
    2>&1 | tee "$UART_LOG"

grep -aq 'K230_NNCASE_RUNTIME_PASS' "$UART_LOG"
grep -aq '^k230_kpu_start ' "$TRACE_FILE"

echo "trace-nncase-runtime: pass"
