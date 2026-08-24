#!/usr/bin/env bash
set -euo pipefail

# Run one Task-3 dual-Guest closed-loop experiment and wait for the controller
# to reach MIN_ELAPSED_MS of loop time, then quit QEMU via QMP.
#
# Usage: run-task3-experiment.sh <label> [ai|baseline]

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
label="${1:?label required}"
mode="${2:?mode required: ai or baseline}"
min_elapsed_ms="${MIN_ELAPSED_MS:-35000}"
workdir="$repo_root/tmp/net-dual-guest"
log="/tmp/task3-${label}.log"
qemu_sock="$workdir/qmp.sock"

case "$mode" in
    ai)       initramfs="$workdir/linux-task2/task2-linux-initramfs-ai.cpio.gz" ;;
    baseline) initramfs="$workdir/linux-task2/task2-linux-initramfs-baseline.cpio.gz" ;;
    *) echo "mode must be ai or baseline" >&2; exit 1 ;;
esac

cp "$initramfs" "$workdir/linux-task2/task2-linux-initramfs.cpio.gz"
rm -f "$qemu_sock" "$workdir/linux.pcap" "$workdir/rtos.pcap"

nohup cargo xtask axvisor qemu \
    --config /tmp/task2-axvisor-qemu-debug.toml \
    --qemu-config scripts/test/net-dual-guest/qemu-aarch64-p2.toml \
    --vmconfigs scripts/test/net-dual-guest/vm-aarch64-p2-linux.toml \
    --vmconfigs scripts/test/net-dual-guest/vm-aarch64-p2-rtos.toml \
    --rootfs "$repo_root/tmp/axbuild/rootfs/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img" \
    > "$log" 2>&1 &
run_pid=$!

deadline=$(( $(date +%s) + 900 ))
while (( $(date +%s) < deadline )); do
    if ! kill -0 "$run_pid" 2>/dev/null; then
        echo "run process exited early; tail of log:"
        tail -5 "$log"
        exit 1
    fi
    if [ -S "$qemu_sock" ]; then
        last_elapsed=$(grep -oE "TASK3_(STATUS_RECEIVED|CONTROL_SENT) elapsed_ms=[0-9]+" "$log" | grep -oE "[0-9]+$" | tail -1 || true)
        if [ -n "$last_elapsed" ] && [ "$last_elapsed" -ge "$min_elapsed_ms" ]; then
            echo "elapsed_ms=$last_elapsed >= $min_elapsed_ms; quitting"
            python3 scripts/test/net-dual-guest/qmp_link.py "$qemu_sock" quit || true
            break
        fi
    fi
    sleep 15
done

sleep 15
pkill -f "tg-xtask axvisor qemu" 2>/dev/null || true
sleep 3
echo "run $label ($mode) finished; log=$log"
