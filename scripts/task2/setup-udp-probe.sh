#!/usr/bin/env bash
# Build icpc-udp-probe and install into Alpine rootfs as /usr/local/bin/icpc-udp-probe.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
IMG="${ROOT}/tmp/axbuild/rootfs/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img"
BIN="${ROOT}/tmp/task2/icpc-udp-probe"
GUEST_PATH="/usr/local/bin/icpc-udp-probe"

"${ROOT}/scripts/task2/build-udp-probe.sh" "${BIN}"

if [[ ! -f "${IMG}" ]]; then
  echo "[task2] rootfs image missing: ${IMG}" >&2
  exit 1
fi

DEBUGFS="$(command -v debugfs || echo /usr/sbin/debugfs)"
echo "[task2] Installing ${BIN} -> ${IMG}:${GUEST_PATH}"
# Ensure parent dir exists (busybox rootfs usually has /usr/local/bin).
"${DEBUGFS}" -w "${IMG}" -R 'mkdir /usr/local' >/dev/null 2>&1 || true
"${DEBUGFS}" -w "${IMG}" -R 'mkdir /usr/local/bin' >/dev/null 2>&1 || true
"${DEBUGFS}" -w "${IMG}" -R "rm ${GUEST_PATH}" >/dev/null 2>&1 || true
"${DEBUGFS}" -w "${IMG}" -R "write ${BIN} ${GUEST_PATH}"
"${DEBUGFS}" -w "${IMG}" -R "set_inode_field ${GUEST_PATH} mode 0x81ed" >/dev/null 2>&1 || true
if ! "${DEBUGFS}" "${IMG}" -R "stat ${GUEST_PATH}" 2>/dev/null | grep -q 'Inode:'; then
  echo "[task2] verify failed: ${GUEST_PATH} missing after write" >&2
  exit 1
fi
echo "[task2] Installed ${GUEST_PATH}"
