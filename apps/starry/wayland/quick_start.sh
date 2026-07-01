#!/usr/bin/env bash
set -euo pipefail

export PATH="/opt/homebrew/opt/e2fsprogs/sbin:$PATH"

workspace="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "$workspace"

arch="riscv64"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --arch)
      if [[ $# -lt 2 ]]; then
        echo "error: --arch requires a value" >&2
        exit 1
      fi
      arch="$2"
      shift 2
      ;;
    -h | --help)
      echo "usage: $0 [--arch riscv64|aarch64]" >&2
      exit 0
      ;;
    *)
      echo "error: unknown argument: $1" >&2
      echo "usage: $0 [--arch riscv64|aarch64]" >&2
      exit 1
      ;;
  esac
done

case "$arch" in
  riscv64)
    target="riscv64gc-unknown-none-elf"
    rootfs_name="rootfs-riscv64-alpine"
    ;;
  aarch64)
    target="aarch64-unknown-none-softfloat"
    rootfs_name="rootfs-aarch64-alpine"
    ;;
  *)
    echo "error: unsupported arch: $arch" >&2
    echo "usage: $0 [--arch riscv64|aarch64]" >&2
    exit 1
    ;;
esac

mkdir -p tmp/wayland-manual

src_img="target/${target}/qemu-cases/starry-apps/rootfs.img"
if [[ ! -f "$src_img" ]]; then
  src_img="tmp/axbuild/rootfs/${rootfs_name}/${rootfs_name}.img"
fi
if [[ ! -f "$src_img" ]]; then
  src_img="tmp/axbuild/rootfs/${rootfs_name}.img/${rootfs_name}.img"
fi
if [[ ! -f "$src_img" ]]; then
  src_img="tmp/axbuild/rootfs/${rootfs_name}.img"
fi
if [[ ! -f "$src_img" ]]; then
  src_img="/tmp/.tgos-images/${rootfs_name}.img/${rootfs_name}.img"
fi
if [[ ! -f "$src_img" ]]; then
  echo "missing ${arch} rootfs image; run: cargo xtask starry app qemu -t wayland --arch ${arch}" >&2
  exit 1
fi

manual_img="tmp/wayland-manual/${arch}.img"
cp "$src_img" "$manual_img"

resolv_conf="tmp/wayland-manual/resolv.conf"
printf 'nameserver 10.0.2.3\n' > "$resolv_conf"
debugfs -w -R "rm /etc/resolv.conf" "$manual_img" >/dev/null 2>&1 || true
debugfs -w -R "write $resolv_conf /etc/resolv.conf" "$manual_img" >/dev/null
debugfs -w -R "sif /etc/resolv.conf mode 0100644" "$manual_img" >/dev/null

VNC_DISPLAY="${VNC_DISPLAY:-30}"
VNC_PORT=$((5900 + VNC_DISPLAY))

python3 - "$arch" "$VNC_DISPLAY" "$rootfs_name" "$manual_img" <<'PY'
from pathlib import Path
import re
import sys

arch, display, rootfs_name, manual_img = sys.argv[1:]
src = Path(f"apps/starry/wayland/qemu-{arch}.toml")
dst = Path(f"tmp/wayland-manual/qemu-{arch}-vnc.toml")
text = src.read_text()
text = text.replace(
    '"-nographic",',
    f'"-serial",\n    "stdio",\n    "-monitor",\n    "none",\n    "-vnc",\n    "127.0.0.1:{display}",',
)
manual_drive = f"file=${{workspace}}/{manual_img}"
text = re.sub(
    rf"file=(?:\${{workspace}}/)?(?:tmp/axbuild/rootfs|/tmp/\.tgos-images)/{re.escape(rootfs_name)}(?:\.img)?/{re.escape(rootfs_name)}\.img",
    manual_drive,
    text,
)
text = text.replace(
    f"file=${{workspace}}/tmp/axbuild/rootfs/{rootfs_name}.img",
    manual_drive,
)
if "virtio-net-pci,netdev=net0" not in text:
    text = text.replace(
        '    "-drive",\n'
        f'    "id=disk0,if=none,format=raw,{manual_drive}",',
        '    "-drive",\n'
        f'    "id=disk0,if=none,format=raw,{manual_drive}",\n'
        '    "-device",\n'
        '    "virtio-net-pci,netdev=net0",\n'
        '    "-netdev",\n'
        '    "user,id=net0,net=10.0.2.0/24,dhcpstart=10.0.2.15",',
    )
text = text.replace('shell_init_cmd = "/usr/bin/wayland-test.sh"', 'shell_init_cmd = "/usr/bin/wayland-quick-start.sh"')
text = text.replace('success_regex = ["(?m)^WAYLAND_TEST_PASSED\\\\s*$"]', 'success_regex = []')
text = re.sub(
    r"fail_regex = \[\n(?:    .*\n)+\]",
    'fail_regex = ["(?i)\\\\bpanic(?:ked)?\\\\b", "(?m)^WAYLAND_QUICK_START_FAILED:"]',
    text,
    count=1,
)
text = re.sub(r"timeout = \d+", "timeout = 900", text, count=1)
if "snapshot =" not in text:
    text += "\nsnapshot = false\n"
dst.write_text(text)
PY

echo "VNC display: 127.0.0.1:${VNC_PORT}"
echo "Architecture: ${arch}"
echo "Guest will auto-run /usr/bin/wayland-quick-start.sh"

cargo xtask starry app qemu \
  -t wayland \
  --arch "$arch" \
  --qemu-config "tmp/wayland-manual/qemu-${arch}-vnc.toml"
