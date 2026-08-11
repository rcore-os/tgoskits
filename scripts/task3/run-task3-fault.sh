#!/usr/bin/env bash
set -euo pipefail

# Task-3 M5 runtime fault-injection experiment.
#
# QEMU `set_link` only toggles the virtio-net link-status bit and does not
# stop the data path in the virtio-mmio passthrough setup used here (neither
# at runtime nor before guest boot), and `netdev_del` cannot be restored
# cleanly because the NIC keeps the old socket peer.  This script therefore
# routes the two guests through the P3 frame relay (`ack_drop_proxy.py`) and
# lets the proxy discard every frame in both directions for a timed blackout
# window.  The blackout is a real runtime data-link outage between the two
# QEMU netdevs: both T2N1 endpoints exhaust retransmission/heartbeat and
# enter Safe, then recover when the window ends.
#
# Usage: run-task3-fault.sh <label>
# Env:   BLACKOUT_START_MS (default 25000), BLACKOUT_DURATION_MS (default 10000)

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
label="${1:?label required}"
workdir="$repo_root/tmp/net-dual-guest"
log="/tmp/task3-${label}.log"
proxy_log="/tmp/task3-${label}-proxy.log"
qemu_sock="$workdir/qmp-p3-final.sock"
qmp="$repo_root/scripts/test/net-dual-guest/qmp_link.py"
proxy="$repo_root/scripts/test/net-dual-guest/ack_drop_proxy.py"
blackout_start_ms="${BLACKOUT_START_MS:-25000}"
blackout_duration_ms="${BLACKOUT_DURATION_MS:-10000}"
proxy_pid=""

cleanup() {
    if [ -n "$proxy_pid" ]; then
        kill "$proxy_pid" 2>/dev/null || true
    fi
    pkill -f "qemu-system-[a]arch64" 2>/dev/null || true
}
trap cleanup EXIT

pkill -f "ack_drop_proxy.py" 2>/dev/null || true
pkill -f "qemu-system-[a]arch64" 2>/dev/null || true
sleep 3

cp "$workdir/linux-task2/task2-linux-initramfs-ai.cpio.gz" \
    "$workdir/linux-task2/task2-linux-initramfs.cpio.gz"
rm -f "$qemu_sock" "$workdir/linux-p3-final.pcap" "$workdir/rtos-p3-final.pcap"

# Start the relay first: both QEMU netdevs connect to it.
# The bounded ACK drop is disabled (drop_count=0): dropping one ACK leaves the
# controller's reliable frame pending and the next CONTROL then fails with
# ReliableFramePending.  The blackout alone is the M5 fault.
nohup python3 "$proxy" \
    --linux-port 12731 --rtos-port 12732 \
    --drop-direction rtos-to-linux --drop-kind ack --drop-count 0 \
    --blackout-start-ms "$blackout_start_ms" \
    --blackout-duration-ms "$blackout_duration_ms" \
    --log "$proxy_log" &
proxy_pid=$!
for _ in $(seq 1 30); do
    if grep -q "PROXY_READY" "$proxy_log" 2>/dev/null; then
        break
    fi
    sleep 1
done
if ! grep -q "PROXY_READY" "$proxy_log" 2>/dev/null; then
    echo "proxy failed to start; tail:"; tail -5 "$proxy_log"; exit 1
fi

nohup cargo xtask axvisor qemu \
    --config /tmp/task2-axvisor-qemu-debug.toml \
    --qemu-config scripts/test/net-dual-guest/qemu-aarch64-p3-proxy-final.toml \
    --vmconfigs scripts/test/net-dual-guest/vm-aarch64-p2-linux.toml \
    --vmconfigs scripts/test/net-dual-guest/vm-aarch64-p2-rtos.toml \
    --rootfs "$repo_root/tmp/axbuild/rootfs/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img" \
    > "$log" 2>&1 &
run_pid=$!

# Only match log lines that appear after the previous wait completed; the
# guest log is append-only and patterns like TASK2_RECOVERED also occur in
# earlier boot sequences, so scanning from a moving offset keeps the evidence
# ordering honest.
offset=0
wait_for_new() {
    local pattern="$1" timeout="${2:-180}"
    local deadline=$(( $(date +%s) + timeout ))
    while (( $(date +%s) < deadline )); do
        local lines
        lines=$(wc -l < "$log")
        if (( lines > offset )) && tail -n +$((offset + 1)) "$log" | grep -qE "$pattern"; then
            offset=$lines
            return 0
        fi
        if ! kill -0 "$run_pid" 2>/dev/null; then
            echo "run process exited early; tail:"
            tail -5 "$log"
            exit 1
        fi
        sleep 3
    done
    echo "timeout waiting for $pattern"
    exit 1
}

echo "[fault] waiting for the control loop to start"
wait_for_new "TASK3_CONTROL_SENT" 300

echo "[fault] control loop running; blackout starts at ${blackout_start_ms}ms after connect"
echo "[fault] waiting for safe state caused by the blackout"
wait_for_new "TASK2_SAFE" 120

echo "[fault] safe state observed; waiting for recovery after blackout ends"
wait_for_new "TASK2_RECOVERED" 120
echo "[fault] waiting for a resumed control loop"
wait_for_new "TASK3_CONTROL_SENT elapsed_ms=" 120
sleep 10

echo "[fault] quitting"
python3 "$qmp" "$qemu_sock" quit || true
sleep 15
kill "$proxy_pid" 2>/dev/null || true
pkill -f "qemu-system-[a]arch64" 2>/dev/null || true
sleep 3
echo "run $label (fault) finished; log=$log proxy_log=$proxy_log"
