#!/usr/bin/env bash
set -euo pipefail

# Task-3 runtime fault-injection experiment on the in-hypervisor
# virtio-net switch. The console driver owns the whole lifecycle: loop
# start, capture on, a timed hypervisor-side blackout (`virtnet drop on/off`)
# that drops every frame in both directions, Safe entry, recovery, pcap
# streaming, and the QMP quit.
#
# The blackout window is anchored to the controller's own elapsed_ms log
# (25s..~35s of loop time, matching the P3-proxy window): both T2N1
# endpoints exhaust retransmission/heartbeat and enter Safe, then
# resynchronize when the gate is lifted.
#
# Usage: run-task3-switch-fault.sh <label> [baseline|cnn|yolo]

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
rootfs="${STARRY_TASK23_ROOTFS:-$repo_root/tmp/axbuild/rootfs/rootfs-aarch64-alpine.img}"
label="${1:?label required}"
mode="${2:-yolo}"
workdir="$repo_root/tmp/net-dual-guest"
log="/tmp/task3-${label}.log"
build_log="/tmp/task3-${label}-build.log"
qemu_sock="$workdir/qmp-switch.sock"
serial_sock="$workdir/serial-switch.sock"
steps="/tmp/task3-${label}.steps"
run_pid=""

cleanup() {
    pkill -f "tg-xtask axvisor qemu" 2>/dev/null || true
    pkill -f "qemu-system-[a]arch64" 2>/dev/null || true
}
trap cleanup EXIT

case "$mode" in
    baseline|cnn|yolo)
        initramfs="$workdir/linux-task2/task2-linux-initramfs-${mode}.cpio.gz"
        ;;
    ai)
        # Keep the historical alias usable while making the selected model
        # explicit in new evidence.
        initramfs="$workdir/linux-task2/task2-linux-initramfs-cnn.cpio.gz"
        mode="cnn"
        ;;
    *)
        echo "mode must be baseline, cnn, or yolo" >&2
        exit 2
        ;;
esac
if [ ! -s "$initramfs" ]; then
    echo "missing or empty initramfs for mode=$mode: $initramfs" >&2
    exit 1
fi

cp "$initramfs" \
    "$workdir/linux-task2/task2-linux-initramfs.cpio.gz"
rm -f "$qemu_sock" "$serial_sock" "$log" \
    "$workdir/switch.vm1.pcap" "$workdir/switch.vm2.pcap"

cat > "$steps" <<EOF
# Wait for the control loop inside the boot-time console multiplex.
expect 420 TASK3_CONTROL_SENT
# Start frame capture, then watch the Linux controller console.
detach
cmd virtnet capture on
expect 20 virtnet: capture ON
attach 1
# Wait until the loop reaches ~25s of elapsed time (25000..29999 ms).
expect 120 elapsed_ms=2[5-9][0-9]{3}
# Engage the blackout; guests keep running, every frame is dropped.
detach
cmd virtnet drop on
expect 20 virtnet: blackout ON
attach 1
# Both endpoints exhaust retransmission and enter Safe.
expect 90 TASK2_SAFE
# Keep the link down for a ~10s blackout window.
hold 10
# Lift the blackout and wait for the T2N1 resynchronization.
detach
cmd virtnet drop off
expect 20 virtnet: blackout OFF
attach 1
expect 90 TASK2_RECOVERED
# Observe resumed closed-loop cycles (>=45s of loop time).
expect 120 elapsed_ms=(4[5-9]|[5-9][0-9]|[1-9][0-9][0-9])[0-9]{3}
detach
dump-pcap $workdir/switch
qmp-quit $qemu_sock
EOF

rm -f "$log"
nohup cargo xtask axvisor qemu \
    --config scripts/test/net-dual-guest/axvisor-qemu-debug.toml \
    --qemu-config scripts/test/net-dual-guest/qemu-aarch64-p2-switch.toml \
    --vmconfigs scripts/test/net-dual-guest/vm-aarch64-p2-switch-linux.toml \
    --vmconfigs scripts/test/net-dual-guest/vm-aarch64-p2-switch-rtos.toml \
    --rootfs "$rootfs" \
    > "$build_log" 2>&1 &
run_pid=$!

for _ in $(seq 1 180); do
    if [ -S "$serial_sock" ]; then
        break
    fi
    if ! kill -0 "$run_pid" 2>/dev/null; then
        echo "axvisor run exited early; tail of build log:"
        tail -20 "$build_log"
        exit 1
    fi
    sleep 2
done
if [ ! -S "$serial_sock" ]; then
    echo "serial socket never appeared; tail of build log:"
    tail -20 "$build_log"
    exit 1
fi

python3 scripts/test/net-dual-guest/serial_console.py \
    "$serial_sock" "$log" --script "$steps"
driver_status=$?

sleep 5
pkill -f "tg-xtask axvisor qemu" 2>/dev/null || true
pkill -f "qemu-system-[a]arch64" 2>/dev/null || true
sleep 2

if [ "$driver_status" -ne 0 ]; then
    echo "console driver failed with status $driver_status; tail of run log:"
    tail -30 "$log"
    exit "$driver_status"
fi

if [ ! -s "$log" ]; then
    echo "missing or empty fault log: $log" >&2
    exit 1
fi
for pcap in "$workdir"/switch.vm1.pcap "$workdir"/switch.vm2.pcap; do
    if [ ! -s "$pcap" ]; then
        echo "missing or empty capture: $pcap" >&2
        exit 1
    fi
done

# Validate the fault contract before archiving it.  The checks deliberately
# require evidence on both sides of the blackout: a Safe event alone is not a
# recovery proof, and a non-empty pcap alone is not a T2N1 proof.
python3 - "$log" <<'PY'
import re
import sys

text = open(sys.argv[1], encoding="utf-8", errors="replace").read()
required = [
    "TASK3_MODEL_READY",
    "virtnet: blackout ON",
    "TASK2_SAFE",
    "virtnet: blackout OFF",
    "TASK2_RECOVERED",
    "TASK3_CONTROL_SENT",
    "TASK3_STATUS_RECEIVED",
    "CAPDUMP_END",
]
missing = [marker for marker in required if marker not in text]
if missing:
    raise SystemExit("fault evidence missing markers: " + ", ".join(missing))

def first(marker: str) -> int:
    match = re.search(re.escape(marker), text)
    assert match is not None
    return match.start()

blackout_on = first("virtnet: blackout ON")
safe = first("TASK2_SAFE")
blackout_off = first("virtnet: blackout OFF")
recovered = first("TASK2_RECOVERED")
capture_end = first("CAPDUMP_END")
if not blackout_on < safe < blackout_off < recovered < capture_end:
    raise SystemExit("fault marker order is invalid")

before_recovery = text[:recovered]
after_recovery = text[recovered:]
if "TASK3_CONTROL_SENT" not in before_recovery:
    raise SystemExit("no YOLO control was sent before recovery")
if "TASK3_STATUS_RECEIVED" not in before_recovery:
    raise SystemExit("no status was received before recovery")
if "TASK3_CONTROL_SENT" not in after_recovery:
    raise SystemExit("YOLO control did not resume after recovery")
if "TASK3_STATUS_RECEIVED" not in after_recovery:
    raise SystemExit("status did not resume after recovery")

elapsed = [int(value) for value in re.findall(r"elapsed_ms=(\d+)", after_recovery)]
if not elapsed or max(elapsed) < 45000:
    raise SystemExit(
        f"recovery run did not continue to 45s elapsed window: max={max(elapsed, default=None)}"
    )
PY

python3 scripts/test/net-dual-guest/verify_pcap.py \
    --tag '' --require-task2 "$workdir/switch.vm1.pcap" "$workdir/switch.vm2.pcap"

echo "run $label mode=$mode (fault) finished; log=$log build_log=$build_log"
ls -la "$workdir"/switch.vm*.pcap

# Archive the evidence under results/task3/switch/fault-<label>/.
out_dir="$repo_root/results/task3/switch/fault-$label"
mkdir -p "$out_dir"
cp "$log" "$out_dir/run.log" 2>/dev/null || true
cp "$build_log" "$out_dir/build.log" 2>/dev/null || true
cp "$workdir/switch.vm1.pcap" "$out_dir/linux.pcap"
cp "$workdir/switch.vm2.pcap" "$out_dir/rtos.pcap"
zephyr_manifest="$workdir/zephyr-task2/manifest.toml"
sha256_or_none() {
    if [ -f "$1" ]; then
        sha256sum "$1" | awk '{print $1}'
    else
        printf 'unavailable'
    fi
}
{
    printf 'label = "%s"\n' "$label"
    printf 'mode = "%s"\n' "$mode"
    printf 'fault = "blackout"\n'
    printf 'blackout_expected = true\n'
    printf 'recovery_elapsed_min_ms = 45000\n'
    printf 'log_sha256 = "%s"\n' "$(sha256sum "$out_dir/run.log" | awk '{print $1}')"
    printf 'build_log_sha256 = "%s"\n' "$(sha256sum "$out_dir/build.log" | awk '{print $1}')"
    printf 'linux_pcap_sha256 = "%s"\n' "$(sha256sum "$out_dir/linux.pcap" | awk '{print $1}')"
    printf 'rtos_pcap_sha256 = "%s"\n' "$(sha256sum "$out_dir/rtos.pcap" | awk '{print $1}')"
    printf 'linux_initramfs_sha256 = "%s"\n' "$(sha256_or_none "$workdir/linux-task2/task2-linux-initramfs-ai.cpio.gz")"
    printf 'zephyr_binary_sha256 = "%s"\n' "$(sha256_or_none "$workdir/zephyr-task2/zephyr-task2.bin")"
    if [ -f "$zephyr_manifest" ]; then
        printf 'zephyr_manifest_sha256 = "%s"\n' "$(sha256sum "$zephyr_manifest" | awk '{print $1}')"
    fi
} > "$out_dir/manifest.toml"
echo "archived evidence under $out_dir"
