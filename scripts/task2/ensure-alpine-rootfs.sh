#!/usr/bin/env bash
# Make sure the managed aarch64 Alpine rootfs exists before debugfs injection.
#
# Task 2/3 setup scripts write guest binaries into
# `tmp/axbuild/rootfs/rootfs-aarch64-alpine.img`. `cargo xtask axvisor test qemu`
# pulls that image later; the wrapper scripts run setup first, so a missing
# file used to fail with "rootfs image missing" before QEMU started.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${ROOT}"
IMG="${ROOT}/tmp/axbuild/rootfs/rootfs-aarch64-alpine.img"

if [[ -f "${IMG}" ]]; then
  echo "[task2] alpine rootfs ready: ${IMG}"
  exit 0
fi

echo "[task2] Pulling managed aarch64 Alpine rootfs..."
cargo xtask image pull rootfs-aarch64-alpine.img

if [[ ! -f "${IMG}" ]]; then
  echo "[task2] expected ${IMG} after image pull" >&2
  exit 1
fi
echo "[task2] alpine rootfs ready: ${IMG}"
