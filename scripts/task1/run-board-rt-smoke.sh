#!/usr/bin/env bash
# Quick board smoke: mixed Linux+RT, 200 samples (no CPU stress loops).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

"${ROOT}/scripts/task1/prepare-board-mixed-rt-guests.sh"

cd "${ROOT}"
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${ROOT}/target}" \
  cargo xtask axvisor test board --board orangepi-5-plus-linux -c board-orangepi-5-plus-mixed-rt-smoke
