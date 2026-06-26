#!/usr/bin/env bash
set -euo pipefail

export PATH="/opt/homebrew/opt/e2fsprogs/sbin:$PATH"

workspace="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "$workspace"

mkdir -p tmp/wayland-manual

src_img="tmp/axbuild/rootfs/rootfs-x86_64-alpine.img/rootfs-x86_64-alpine.img"
if [[ ! -f "$src_img" ]]; then
  src_img="tmp/axbuild/rootfs/rootfs-x86_64-alpine/rootfs-x86_64-alpine.img"
fi
if [[ ! -f "$src_img" ]]; then
  src_img="tmp/axbuild/rootfs/rootfs-x86_64-alpine.img"
fi
if [[ ! -f "$src_img" ]]; then
  echo "missing x86_64 rootfs image; run: cargo xtask starry app qemu -t wayland --arch x86_64" >&2
  exit 1
fi

cp "$src_img" "tmp/wayland-manual/x86_64.img"

VNC_DISPLAY="${VNC_DISPLAY:-30}"
VNC_PORT=$((5900 + VNC_DISPLAY))

python3 - "$VNC_DISPLAY" <<'PY'
from pathlib import Path
import sys

display = sys.argv[1]
src = Path("apps/starry/wayland/qemu-x86_64-vnc.toml")
dst = Path("tmp/wayland-manual/qemu-x86_64-vnc.toml")
text = src.read_text()
text = text.replace('"127.0.0.1:30"', f'"127.0.0.1:{display}"')
text = text.replace(
    "file=${workspace}/tmp/axbuild/rootfs/rootfs-x86_64-alpine.img/rootfs-x86_64-alpine.img",
    "file=${workspace}/tmp/wayland-manual/x86_64.img",
)
text = text.replace(
    "file=${workspace}/tmp/axbuild/rootfs/rootfs-x86_64-alpine/rootfs-x86_64-alpine.img",
    "file=${workspace}/tmp/wayland-manual/x86_64.img",
)
dst.write_text(text)
PY

echo "VNC display: 127.0.0.1:${VNC_PORT}"
echo "Guest will auto-run /usr/bin/wayland-quick-start.sh"

cargo xtask starry app qemu \
  -t wayland \
  --arch x86_64 \
  --qemu-config tmp/wayland-manual/qemu-x86_64-vnc.toml
