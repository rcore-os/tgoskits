#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

cat >&2 <<'EOF'
prepare_rootfs.sh no longer writes directly into rootfs images.

Use build_rootfs.sh to pull/resize the managed base image and prepare the
toolchain overlay cache, then launch through xtask so the existing Starry app
overlay injection path is used:

  apps/starry/macos-selfbuild/build_rootfs.sh
  cargo xtask starry app qemu -t macos-selfbuild --arch aarch64 \
    --qemu-config apps/starry/macos-selfbuild/qemu-aarch64-hvf.toml
EOF

exec "$script_dir/build_rootfs.sh" "$@"
