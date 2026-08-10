#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
IMG="${ROOT}/tmp/axbuild/rootfs/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img"
BIN="${ROOT}/tmp/task2/icpc-bench-client"
GUEST_PATH="/usr/local/bin/icpc-bench"

"${ROOT}/scripts/task2/build-icpc-bench.sh" "${BIN}"

if [[ ! -f "${IMG}" ]]; then
  echo "[task2] rootfs image missing: ${IMG}" >&2
  exit 1
fi

DEBUGFS="$(command -v debugfs || echo /usr/sbin/debugfs)"
echo "[task2] Installing ${BIN} -> ${IMG}:${GUEST_PATH}"
"${DEBUGFS}" -w "${IMG}" -R 'mkdir /usr/local' >/dev/null 2>&1 || true
"${DEBUGFS}" -w "${IMG}" -R 'mkdir /usr/local/bin' >/dev/null 2>&1 || true
"${DEBUGFS}" -w "${IMG}" -R "rm ${GUEST_PATH}" >/dev/null 2>&1 || true
"${DEBUGFS}" -w "${IMG}" -R "write ${BIN} ${GUEST_PATH}"
"${DEBUGFS}" -w "${IMG}" -R "set_inode_field ${GUEST_PATH} mode 0x81ed" >/dev/null 2>&1 || true
echo "[task2] Installed ${GUEST_PATH}"
