#!/usr/bin/env bash
# Run the cyclictest-style periodic wake-up latency measurement on ArceOS under
# QEMU and print the jitter statistics. Run it bare-metal for a baseline; the
# same measurement can later run inside an Axvisor guest to quantify the
# virtualization-added latency.
#
# The pass/fail run goes through `cargo xtask`, which discards every serial line
# except the success/fail pattern matches (ostool's output matcher). The
# cyclictest statistics line is therefore captured by re-running the exact QEMU
# command from the xtask log with the serial console redirected to a file.
#
# Usage: scripts/rt-latency/run_cyclictest.sh [target]
set -euo pipefail

TARGET="${1:-loongarch64-unknown-none-softfloat}"
LOG_DIR="$(mktemp -d)"
LOG="$LOG_DIR/cyclictest.log"
SERIAL_LOG="$LOG_DIR/serial.log"
QEMU_STDOUT="$LOG_DIR/qemu.stdout"
trap 'rm -rf "$LOG_DIR"' EXIT

# mkfs.fat (dosfstools) is required by the FAT32 rootfs generation and is not
# on PATH by default.
export PATH="/opt/homebrew/sbin:$PATH"

echo "==> running task-cyclictest (target=${TARGET}, group=rust)"
if ! cargo xtask arceos test qemu \
    --test-group rust \
    --test-case task-cyclictest \
    --target "$TARGET" 2>&1 | tee "$LOG"; then
    echo "==> FAIL (tail of output:)"
    tail -40 "$LOG"
    exit 1
fi

# Re-run the exact QEMU command printed by xtask, with the serial console
# redirected to a file, so the cyclictest statistics can be observed.
QEMU_CMD="$(grep -m1 -o 'qemu-system-loongarch64 .*' "$LOG" || true)"
if [ -z "$QEMU_CMD" ]; then
    echo "==> FAIL: could not extract the QEMU command from the xtask log" >&2
    exit 1
fi
QEMU_CMD="${QEMU_CMD/-serial mon:stdio/-serial file:'$SERIAL_LOG'}"

echo "==> re-running QEMU to capture serial statistics"
eval "$QEMU_CMD" </dev/null >"$QEMU_STDOUT" 2>&1 &
QEMU_PID=$!

STATS=""
for _ in $(seq 1 120); do
    if [ -s "$SERIAL_LOG" ] \
        && STATS="$(grep -m1 'cyclictest:' "$SERIAL_LOG" | tr -d '\r' || true)" \
        && [ -n "$STATS" ]; then
        break
    fi
    if ! kill -0 "$QEMU_PID" 2>/dev/null; then
        echo "==> QEMU exited before printing statistics:" >&2
        tail -20 "$QEMU_STDOUT" >&2 || true
        exit 1
    fi
    sleep 1
done
kill "$QEMU_PID" 2>/dev/null || true
wait "$QEMU_PID" 2>/dev/null || true

if [ -n "$STATS" ]; then
    echo "==> PASS (task-cyclictest)"
    echo "==> jitter statistics:"
    echo "$STATS"
else
    echo "==> FAIL: cyclictest statistics not captured" >&2
    tail -20 "$SERIAL_LOG" >&2 || true
    exit 1
fi
