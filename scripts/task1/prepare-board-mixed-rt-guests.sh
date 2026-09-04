#!/usr/bin/env bash
# Build (and optionally deploy) Task1 board guest assets for mixed-rt AxVisor cases.
#
# Prerequisites on the board rootfs (usually from golden image):
#   /guest/linux/orangepi-5-plus
#   /guest/linux/initramfs.cpio
#
# This script always builds the RT flat image; when BOARD_IP is set it also runs
# deploy-board-rt-guest.sh to install:
#   /guest/arceos/orangepi-5-plus-rt-latency
#
# Usage:
#   ./scripts/task1/prepare-board-mixed-rt-guests.sh
#   BOARD_IP=192.168.x.x ./scripts/task1/prepare-board-mixed-rt-guests.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

info() { echo "[task1-board-prep] $*"; }

info "Building board RT guest flat image..."
RT_LATENCY_FEATURES="rt-latency,rt-latency-guest" \
  "${ROOT}/os/axvisor/scripts/task1/build-arceos-rt-guest-board.sh"

if [[ -n "${BOARD_IP:-}" ]]; then
  info "Deploying RT guest to ${BOARD_IP}..."
  "${ROOT}/scripts/task1/deploy-board-rt-guest.sh" "${BOARD_IP}"
else
  info "BOARD_IP unset — skipped deploy; board must already contain /guest/arceos/orangepi-5-plus-rt-latency"
fi

info "Ready for: cargo xtask axvisor test board --board orangepi-5-plus-linux -c board-orangepi-5-plus-mixed-rt-smoke"
