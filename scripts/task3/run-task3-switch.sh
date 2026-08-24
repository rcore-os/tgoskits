#!/usr/bin/env bash
set -euo pipefail

# Run one Task-3 dual-Guest closed-loop experiment on the in-hypervisor
# virtio-net switch. The console driver owns the whole lifecycle: it waits
# for the loop, enables frame capture, runs until the controller reaches
# MIN_ELAPSED_MS, streams the captured frames out as pcap, and quits QEMU
# over QMP.
#
# Usage: run-task3-switch.sh <label> <baseline|cnn|yolo>
# Env:    MIN_ELAPSED_MS (default 35000)

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
rootfs="${STARRY_TASK23_ROOTFS:-$repo_root/tmp/axbuild/rootfs/rootfs-aarch64-alpine.img}"
label="${1:?label required}"
mode="${2:?mode required: ai or baseline}"
min_elapsed_ms="${MIN_ELAPSED_MS:-35000}"
if ! [[ "$min_elapsed_ms" =~ ^[0-9]+$ ]]; then
    echo "MIN_ELAPSED_MS must be a non-negative integer" >&2
    exit 2
fi
# Generate a Python-regex lower bound for the elapsed_ms marker.
elapsed_pattern="$(python3 - "$min_elapsed_ms" <<'PY'
import re
import sys

value = int(sys.argv[1])
if value == 0:
    print(r"[0-9]+")
    raise SystemExit

digits = str(value)
length = len(digits)
alternatives = [re.escape(digits)]
for index, digit in enumerate(digits):
    if digit == "9":
        continue
    next_digit = str(int(digit) + 1)
    remaining = length - index - 1
    suffix = rf"\d{{{remaining}}}" if remaining else ""
    alternatives.append(re.escape(digits[:index]) + f"[{next_digit}-9]" + suffix)
alternatives.append(rf"[1-9]\d{{{length},}}")
print("(?:" + "|".join(alternatives) + ")")
PY
)"
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
        # Backward-compatible alias for the historical CNN artifact.
        initramfs="$workdir/linux-task2/task2-linux-initramfs-cnn.cpio.gz"
        ;;
    *) echo "mode must be baseline, cnn, or yolo" >&2; exit 1 ;;
esac
if [ ! -s "$initramfs" ]; then
    echo "missing or empty initramfs for mode=$mode: $initramfs" >&2
    exit 1
fi

cp "$initramfs" "$workdir/linux-task2/task2-linux-initramfs.cpio.gz"
rm -f "$qemu_sock" "$serial_sock" "$log" \
    "$workdir/switch.vm1.pcap" "$workdir/switch.vm2.pcap"

capture_ready_marker="TASK3_CONTROL_SENT"
post_capture_expect=""
if [[ "$mode" == "yolo" ]]; then
    # YOLO inference is long enough to arm capture after model initialization
    # and still retain the first CONTROL/ACK/STATUS transaction.
    capture_ready_marker="TASK3_MODEL_READY"
    post_capture_expect="expect 120 TASK3_CONTROL_SENT"
fi

cat > "$steps" <<EOF
# Wait until the control loop is ready inside the boot-time console multiplex.
expect 420 ${capture_ready_marker}
# Start frame capture, then watch the Linux controller console.
detach
cmd virtnet capture on
expect 20 virtnet: capture ON
attach 1
${post_capture_expect}
# Hold until the controller reports MIN_ELAPSED_MS of loop time.
expect 120 elapsed_ms=${elapsed_pattern}
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
    "$serial_sock" "$log" --script "$steps" --verbose \
    2>> "$build_log"
driver_status=$?

sleep 5
pkill -f "tg-xtask axvisor qemu" 2>/dev/null || true
pkill -f "qemu-system-[a]arch64" 2>/dev/null || true
sleep 2

if [ "$driver_status" -ne 0 ]; then
    echo "console driver failed with status $driver_status; tail of run log:"
    tail -20 "$log"
    exit "$driver_status"
fi

python3 - "$log" "$min_elapsed_ms" <<'PY'
import re
import sys

log_path, minimum = sys.argv[1], int(sys.argv[2])
text = open(log_path, encoding="utf-8", errors="replace").read()
values = [int(value) for value in re.findall(r"elapsed_ms=(\d+)", text)]
if not values or max(values) < minimum:
    raise SystemExit(
        f"run did not reach MIN_ELAPSED_MS={minimum}: "
        f"max={max(values, default=None)}"
    )
PY

echo "run $label ($mode) finished; log=$log build_log=$build_log"
for pcap in "$workdir"/switch.vm1.pcap "$workdir"/switch.vm2.pcap; do
    if [ ! -s "$pcap" ]; then
        echo "missing or empty capture: $pcap" >&2
        exit 1
    fi
done
python3 scripts/test/net-dual-guest/verify_pcap.py \
    --tag '' --require-task2 "$workdir/switch.vm1.pcap"
python3 scripts/test/net-dual-guest/verify_pcap.py \
    --tag '' --require-task2 "$workdir/switch.vm2.pcap"
ls -la "$workdir"/switch.vm*.pcap

# Archive the evidence under results/task3/switch/<label>/ so the next run's
# cleanup cannot overwrite it.
out_dir="$repo_root/results/task3/switch/$label"
mkdir -p "$out_dir"
cp "$log" "$out_dir/run.log" 2>/dev/null || true
cp "$build_log" "$out_dir/build.log" 2>/dev/null || true
cp "$workdir/switch.vm1.pcap" "$out_dir/linux.pcap" 2>/dev/null || true
cp "$workdir/switch.vm2.pcap" "$out_dir/rtos.pcap" 2>/dev/null || true
zephyr_manifest="$workdir/zephyr-task2/manifest.toml"
{
    printf 'label = "%s"\n' "$label"
    printf 'mode = "%s"\n' "$mode"
    printf 'min_elapsed_ms = %s\n' "$min_elapsed_ms"
    printf 'linux_pcap_sha256 = "%s"\n' "$(sha256sum "$out_dir/linux.pcap" | awk '{print $1}')"
    printf 'rtos_pcap_sha256 = "%s"\n' "$(sha256sum "$out_dir/rtos.pcap" | awk '{print $1}')"
    if [ -f "$zephyr_manifest" ]; then
        printf 'zephyr_manifest = "%s"\n' "$zephyr_manifest"
        printf 'zephyr_binary_sha256 = "%s"\n' "$(awk -F'"' '/^sha256 = / {print $2}' "$zephyr_manifest")"
    fi
} > "$out_dir/manifest.toml"
echo "archived evidence under $out_dir"
