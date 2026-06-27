#!/usr/bin/env bash
set -euo pipefail

export PATH="/opt/homebrew/opt/e2fsprogs/sbin:$PATH"

workspace="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "$workspace"

mkdir -p tmp/wayland-manual

src_img="tmp/axbuild/rootfs/rootfs-riscv64-alpine/rootfs-riscv64-alpine.img"
if [[ ! -f "$src_img" ]]; then
  src_img="tmp/axbuild/rootfs/rootfs-riscv64-alpine.img/rootfs-riscv64-alpine.img"
fi
if [[ ! -f "$src_img" ]]; then
  src_img="tmp/axbuild/rootfs/rootfs-riscv64-alpine.img"
fi
if [[ ! -f "$src_img" ]]; then
  echo "missing riscv64 rootfs image; run: cargo xtask starry app qemu -t wayland --arch riscv64" >&2
  exit 1
fi

cp "$src_img" "tmp/wayland-manual/riscv64.img"

VNC_DISPLAY="${VNC_DISPLAY:-30}"
VNC_PORT=$((5900 + VNC_DISPLAY))

python3 - "$VNC_DISPLAY" <<'PY'
from pathlib import Path
import re
import sys

display = sys.argv[1]
src = Path("apps/starry/wayland/qemu-riscv64.toml")
dst = Path("tmp/wayland-manual/qemu-riscv64-vnc.toml")
text = src.read_text()
text = text.replace(
    '"-nographic",',
    f'"-serial",\n    "stdio",\n    "-monitor",\n    "none",\n    "-vnc",\n    "127.0.0.1:{display}",',
)
text = text.replace(
    "file=${workspace}/tmp/axbuild/rootfs/rootfs-riscv64-alpine/rootfs-riscv64-alpine.img",
    "file=${workspace}/tmp/wayland-manual/riscv64.img",
)
text = text.replace(
    "file=${workspace}/tmp/axbuild/rootfs/rootfs-riscv64-alpine.img/rootfs-riscv64-alpine.img",
    "file=${workspace}/tmp/wayland-manual/riscv64.img",
)
text = text.replace('shell_init_cmd = "/usr/bin/wayland-test.sh"', 'shell_init_cmd = "/usr/bin/wayland-quick-start.sh"')
text = text.replace('success_regex = ["(?m)^WAYLAND_TEST_PASSED\\\\s*$"]', 'success_regex = []')
text = re.sub(
    r"fail_regex = \[\n(?:    .*\n)+\]",
    'fail_regex = ["(?i)\\\\bpanic(?:ked)?\\\\b", "(?m)^WAYLAND_QUICK_START_FAILED:"]',
    text,
    count=1,
)
text = text.replace("timeout = 600", "timeout = 900\nsnapshot = false")
dst.write_text(text)
PY

echo "VNC display: 127.0.0.1:${VNC_PORT}"
echo "Guest will auto-run /usr/bin/wayland-quick-start.sh"

cargo xtask starry app qemu \
  -t wayland \
  --arch riscv64 \
  --qemu-config tmp/wayland-manual/qemu-riscv64-vnc.toml
