#!/usr/bin/env bash
# Install /icpc-peer-init.sh into the Alpine aarch64 rootfs used by AxVisor tests.
# Uses debugfs (no root required) when the image is not mounted.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
IMG_DIR="${ROOT}/tmp/axbuild/rootfs/rootfs-aarch64-alpine.img"
IMG="${IMG_DIR}/rootfs-aarch64-alpine.img"
SCRIPT_SRC="${ROOT}/scripts/task2/icpc-peer-init.sh"

if [[ ! -f "${IMG}" ]]; then
  echo "[task2] rootfs image missing: ${IMG}" >&2
  echo "[task2] run any axvisor qemu linux test once to extract it" >&2
  exit 1
fi

if [[ ! -x /usr/sbin/debugfs && ! -x "$(command -v debugfs)" ]]; then
  echo "[task2] debugfs not found; cannot install peer init without mounting" >&2
  exit 1
fi

DEBUGFS="$(command -v debugfs || echo /usr/sbin/debugfs)"
echo "[task2] Installing icpc-peer-init.sh into ${IMG} via debugfs"

# Remove stale inode if present, then write. Mode from write is typically 0775.
"${DEBUGFS}" -w "${IMG}" -R 'rm /icpc-peer-init.sh' >/dev/null 2>&1 || true
"${DEBUGFS}" -w "${IMG}" -R "write ${SCRIPT_SRC} /icpc-peer-init.sh"
# Ensure owner-execute bit (debugfs has no chmod; use set_inode_field).
"${DEBUGFS}" -w "${IMG}" -R 'set_inode_field /icpc-peer-init.sh mode 0x81ed' >/dev/null 2>&1 || true
# Verify
if ! "${DEBUGFS}" "${IMG}" -R 'stat /icpc-peer-init.sh' 2>/dev/null | grep -q 'Inode:'; then
  echo "[task2] verify failed: /icpc-peer-init.sh missing after write" >&2
  exit 1
fi
echo "[task2] Installed /icpc-peer-init.sh"
