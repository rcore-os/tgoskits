#!/usr/bin/env bash
# Quick board smoke: mixed Linux+RT, 200 samples (no CPU stress loops).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

echo "[task1-board] Building smoke RT guest (200 samples)..."
RT_LATENCY_FEATURES="rt-latency,rt-latency-guest" \
  "${ROOT}/os/axvisor/scripts/task1/build-arceos-rt-guest-board.sh"

if [[ -n "${BOARD_IP:-}" ]]; then
  "${ROOT}/scripts/task1/deploy-board-rt-guest.sh" "${BOARD_IP}"
fi

cd "${ROOT}"
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${ROOT}/target}" \
  cargo xtask axvisor test board --board orangepi-5-plus-linux -c board-orangepi-5-plus-mixed-rt-smoke
