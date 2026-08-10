#!/usr/bin/env bash
# Build peer initramfs and install it into the AxVisor Alpine rootfs as
# /guest/linux/peer-initramfs.cpio (loaded by linux-net-b.toml).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
IMG="${ROOT}/tmp/axbuild/rootfs/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img"
CPIO="${ROOT}/tmp/task2/peer-initramfs.cpio"
GUEST_PATH="/guest/linux/peer-initramfs.cpio"

"${ROOT}/scripts/task2/build-peer-initramfs.sh" "${CPIO}"

if [[ ! -f "${IMG}" ]]; then
  echo "[task2] rootfs image missing: ${IMG}" >&2
  exit 1
fi

DEBUGFS="$(command -v debugfs || echo /usr/sbin/debugfs)"
echo "[task2] Installing ${CPIO} -> ${IMG}:${GUEST_PATH}"
"${DEBUGFS}" -w "${IMG}" -R "rm ${GUEST_PATH}" >/dev/null 2>&1 || true
"${DEBUGFS}" -w "${IMG}" -R "write ${CPIO} ${GUEST_PATH}"
if ! "${DEBUGFS}" "${IMG}" -R "stat ${GUEST_PATH}" 2>/dev/null | grep -q 'Inode:'; then
  echo "[task2] verify failed: ${GUEST_PATH} missing after write" >&2
  exit 1
fi
echo "[task2] Installed ${GUEST_PATH}"
