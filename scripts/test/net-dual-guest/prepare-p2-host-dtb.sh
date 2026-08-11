#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
qemu_bin="${QEMU_AARCH64:-/home/huhu/.local/bin/qemu-system-aarch64}"
out_dir="${OUT_DIR:-$repo_root/tmp/net-dual-guest}"
base_dtb="$out_dir/qemu-p2-host-base.dtb"
carved_dtb="$out_dir/qemu-p2-host-carved.dtb"
carveout_tool="$repo_root/scripts/test/net-dual-guest/carveout_host_dtb.py"

if [[ ! -x "$qemu_bin" ]]; then
    printf 'error: QEMU is not executable: %s\n' "$qemu_bin" >&2
    exit 1
fi
if [[ ! -f "$carveout_tool" ]]; then
    printf 'error: carveout tool is missing: %s\n' "$carveout_tool" >&2
    exit 1
fi
mkdir -p "$out_dir"

# Dump the host DTB from the same virtio-MMIO topology as P2.  User-mode
# backends are sufficient for DTB generation; the real run replaces them with
# the point-to-point socket backends in qemu-aarch64-p2.toml.
"$qemu_bin" \
    -nographic \
    -cpu cortex-a72 \
    -machine "virt,virtualization=on,gic-version=3,dumpdtb=$base_dtb,dtb-randomness=off" \
    -global virtio-mmio.force-legacy=false \
    -smp 1 \
    -m 8g \
    -netdev user,id=net-linux \
    -device virtio-net-device,netdev=net-linux,mac=52:54:00:12:34:01 \
    -netdev user,id=net-rtos \
    -device virtio-net-device,netdev=net-rtos,mac=52:54:00:12:34:02 \
    >/dev/null 2>&1 || true

python3 "$carveout_tool" \
    "$base_dtb" "$carved_dtb" \
    0x80000000:0x20000000 \
    0xA0000000:0x20000000

sha256sum "$carved_dtb" | awk '{print "host_dtb_sha256=" $1}'
printf 'host_dtb=%s\n' "$carved_dtb"
