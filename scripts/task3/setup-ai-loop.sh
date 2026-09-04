#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
IMG="${ROOT}/tmp/axbuild/rootfs/rootfs-aarch64-alpine.img"
BIN="${ROOT}/tmp/task2/task3-ai-loop-client"
GUEST_PATH="/usr/local/bin/task3-ai-loop"

"${ROOT}/scripts/task3/build-ai-loop.sh" "${BIN}"
"${ROOT}/scripts/task2/ensure-alpine-rootfs.sh"

DEBUGFS="$(command -v debugfs || echo /usr/sbin/debugfs)"
"${DEBUGFS}" -w "${IMG}" -R 'mkdir /usr/local/bin' >/dev/null 2>&1 || true
"${DEBUGFS}" -w "${IMG}" -R "rm ${GUEST_PATH}" >/dev/null 2>&1 || true
"${DEBUGFS}" -w "${IMG}" -R "write ${BIN} ${GUEST_PATH}"
"${DEBUGFS}" -w "${IMG}" -R "set_inode_field ${GUEST_PATH} mode 0x81ed" >/dev/null 2>&1 || true
echo "[task3] Installed ${GUEST_PATH}"
