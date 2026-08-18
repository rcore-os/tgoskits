#!/usr/bin/env bash
# Build the cyclictest ArceOS app and measure periodic wake-up latency both
# bare-metal and inside an Axvisor guest, then print both jitter statistics.
# The delta between the two statistics lines is the scheduling latency the
# hypervisor adds on top of the bare-metal scheduler.
#
# Usage: apps/arceos/cyclictest/run.sh
set -euo pipefail

workspace=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
cd "$workspace"

TARGET=aarch64-unknown-none-softfloat
BUILD_CONFIG=apps/arceos/build-aarch64-cyclictest.toml
BOARD_CONFIG=os/axvisor/configs/board/qemu-aarch64-cyclictest.toml
OUT=target/aarch64-unknown-linux-musl/release/arceos-cyclictest.bin

WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT

objcopy="${LLVM_OBJCOPY:-}"
if [[ -z "$objcopy" ]]; then
    local_host_triple="$(rustc -vV | sed -n 's/^host: //p')"
    objcopy="$(rustc --print sysroot)/lib/rustlib/${local_host_triple}/bin/llvm-objcopy"
fi
if [[ ! -x "$objcopy" ]]; then
    # Fall back to the Linux-host path used by virtio-net-peer/run.sh.
    objcopy="$(rustc --print sysroot)/lib/rustlib/x86_64-unknown-linux-gnu/bin/llvm-objcopy"
fi
if [[ ! -x "$objcopy" ]]; then
    echo "llvm-objcopy not found: $objcopy (set LLVM_OBJCOPY)" >&2
    exit 1
fi

echo "==> building arceos-cyclictest (target=${TARGET})"
cargo xtask arceos build -p arceos-cyclictest -c "$BUILD_CONFIG"
"$objcopy" --strip-all -O binary \
    target/aarch64-unknown-linux-musl/release/arceos-cyclictest \
    "$OUT"

# QEMU config with the serial console redirected to a file. Bare-metal uses
# the plain `virt` machine, the guest needs `virtualization=on`.
capture_qemu_config() {
    local serial="$1"
    local machine="$2"
    local config="$WORK_DIR/qemu-capture.toml"
    cat > "$config" <<EOF
args = [
  "-display", "none",
  "-serial", "file:$serial",
  "-cpu", "cortex-a72",
  "-machine", "$machine",
  "-smp", "4",
  "-m", "4g",
]
fail_regex = ["(?i)panic"]
success_regex = ["cyclictest:"]
timeout = 300
to_bin = true
uefi = false
EOF
    echo "$config"
}

# Run a command, wait for the cyclictest statistics line on the serial
# console, then terminate the spawned QEMU and runner.
capture_stats() {
    local desc="$1"; shift
    local serial="$1"; shift
    local log="$WORK_DIR/$desc.log"
    local pid
    "$@" >"$log" 2>&1 &
    pid=$!
    local stats=""
    for _ in $(seq 1 300); do
        if ! kill -0 "$pid" 2>/dev/null; then
            stats="$(grep -m1 'cyclictest:' "$serial" | tr -d '\r' || true)"
            [ -n "$stats" ] && break
            echo "==> FAIL ($desc): runner exited without statistics" >&2
            tail -40 "$log" >&2 || true
            return 1
        fi
        stats="$(grep -m1 'cyclictest:' "$serial" | tr -d '\r' || true)"
        [ -n "$stats" ] && break
        sleep 1
    done
    if [ -z "$stats" ]; then
        echo "==> FAIL ($desc): statistics not captured within timeout" >&2
        tail -40 "$log" >&2 || true
        return 1
    fi
    # Stop QEMU (matched by its unique serial file path) and the runner.
    local qemu_pid
    qemu_pid="$(pgrep -f "$serial" | head -1 || true)"
    [ -n "$qemu_pid" ] && kill "$qemu_pid" 2>/dev/null || true
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    echo "$stats"
}

echo "==> bare-metal baseline"
BARE_SERIAL="$WORK_DIR/bare.serial"
BARE_QEMU="$(capture_qemu_config "$BARE_SERIAL" "virt,gic-version=3")"
BARE_STATS="$(capture_stats bare "$BARE_SERIAL" \
    cargo xtask arceos qemu -p arceos-cyclictest -c "$BUILD_CONFIG" --qemu-config "$BARE_QEMU")"

echo "==> Axvisor guest"
GUEST_SERIAL="$WORK_DIR/guest.serial"
GUEST_QEMU="$(capture_qemu_config "$GUEST_SERIAL" "virt,virtualization=on,gic-version=3")"
GUEST_STATS="$(capture_stats guest "$GUEST_SERIAL" \
    cargo xtask axvisor qemu --config "$BOARD_CONFIG" --qemu-config "$GUEST_QEMU")"

echo
echo "==> PASS"
echo "  bare-metal: $BARE_STATS"
echo "  guest:      $GUEST_STATS"
