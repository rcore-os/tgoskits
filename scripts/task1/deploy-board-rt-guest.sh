#!/usr/bin/env bash
# Deploy Task 1 RT guest image to OrangePi-5-Plus Linux rootfs for AxVisor board tests.
#
# Prerequisites:
#   1. ./os/axvisor/scripts/task1/build-arceos-rt-guest-board.sh
#   2. Board reachable via SSH (see board-linux-starry-debug skill)
#
# Usage:
#   ./scripts/task1/deploy-board-rt-guest.sh [board-ip]
#   BOARD_IP=192.168.x.x ./scripts/task1/deploy-board-rt-guest.sh
#
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
IMAGE="${ROOT}/os/axvisor/images/orangepi_5_plus_arceos_rt/orangepi-5-plus-rt-latency"
REMOTE_PATH="/guest/arceos/orangepi-5-plus-rt-latency"
BOARD_USER="${BOARD_USER:-orangepi}"
BOARD_IP="${1:-${BOARD_IP:-}}"

info() { echo "[task1-deploy] $*"; }
die() { echo "[task1-deploy] ERROR: $*" >&2; exit 1; }

if [[ ! -f "${IMAGE}" ]]; then
  die "missing ${IMAGE}; run os/axvisor/scripts/task1/build-arceos-rt-guest-board.sh first"
fi

if [[ -z "${BOARD_IP}" ]]; then
  die "set BOARD_IP or pass board IP as first argument"
fi

info "Building RT guest if stale..."
"${ROOT}/os/axvisor/scripts/task1/build-arceos-rt-guest-board.sh"

STAGING="/tmp/tgoskits-rt-latency-$$"
info "Copying to ${BOARD_USER}@${BOARD_IP}:${REMOTE_PATH} ..."
ssh -o BatchMode=yes -o StrictHostKeyChecking=no -o ConnectTimeout=15 \
  "${BOARD_USER}@${BOARD_IP}" "mkdir -p ${STAGING}"
scp -o BatchMode=yes -o StrictHostKeyChecking=no \
  "${IMAGE}" "${BOARD_USER}@${BOARD_IP}:${STAGING}/orangepi-5-plus-rt-latency"

ssh -o BatchMode=yes -o StrictHostKeyChecking=no "${BOARD_USER}@${BOARD_IP}" "
  set -e
  printf '%s\n' '${BOARD_USER}' | sudo -S mkdir -p /guest/arceos
  printf '%s\n' '${BOARD_USER}' | sudo -S install -m 0644 ${STAGING}/orangepi-5-plus-rt-latency ${REMOTE_PATH}
  printf '%s\n' '${BOARD_USER}' | sudo -S sync
  ls -l ${REMOTE_PATH}
  sync
  rm -rf ${STAGING}
"

info "Deployed ${REMOTE_PATH} on ${BOARD_IP}"
info "Run board test: cargo xtask axvisor test board -g stress --board orangepi-5-plus-linux -c board-orangepi-5-plus-mixed-rt-stress-round1-opt-short"
