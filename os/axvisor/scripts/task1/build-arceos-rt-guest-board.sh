#!/usr/bin/env bash
# Build ArceOS rt-latency guest flat image for AxVisor board VM (fs load).
#
# Output: os/axvisor/images/orangepi_5_plus_arceos_rt/orangepi-5-plus-rt-latency
# Deploy to board rootfs: /guest/arceos/orangepi-5-plus-rt-latency
#
# Usage:
#   RT_LATENCY_FEATURES=rt-latency,rt-latency-guest,rt-latency-stress-short ./build-arceos-rt-guest-board.sh
#   RT_LATENCY_FEATURES=rt-latency,rt-latency-guest ./build-arceos-rt-guest-board.sh  # smoke (200 samples)
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
AXVISOR_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
TGOSKITS_ROOT="$(cd "${AXVISOR_ROOT}/../.." && pwd)"
ARCEOS_ROOT="${TGOSKITS_ROOT}/os/arceos"
APP_DIR="${TGOSKITS_ROOT}/test-suit/arceos/rust"
BOARD="${BOARD:-orangepi-5-plus}"
IMAGE_DIR="${AXVISOR_ROOT}/images/orangepi_5_plus_arceos_rt"
OUTPUT_NAME="orangepi-5-plus-rt-latency"
APP_FEATURES="${RT_LATENCY_FEATURES:-rt-latency,rt-latency-guest,rt-latency-stress-short}"

info() { echo "[task1-board] $*"; }

info "Building bare-metal ArceOS rt-latency for board ${BOARD} (APP_FEATURES=${APP_FEATURES})..."
cd "${ARCEOS_ROOT}"
make A="${APP_DIR}" ARCH=aarch64 SMP=1 LOG=info APP_FEATURES="${APP_FEATURES}"

BIN_SRC="${APP_DIR}/rust_aarch64.bin"
if [[ ! -f "${BIN_SRC}" ]]; then
  echo "rt-latency guest binary not found at ${BIN_SRC}" >&2
  exit 1
fi

mkdir -p "${IMAGE_DIR}"
install -m 0644 "${BIN_SRC}" "${IMAGE_DIR}/${OUTPUT_NAME}"

info "Installed ${IMAGE_DIR}/${OUTPUT_NAME} ($(stat -c '%s' "${IMAGE_DIR}/${OUTPUT_NAME}") bytes)"
info "Deploy: ./scripts/task1/deploy-board-rt-guest.sh [board-ip]"
info "VM configs: configs/vms/orangepi-5-plus/arceos-rt-smp1*.toml (image_location=fs)"
