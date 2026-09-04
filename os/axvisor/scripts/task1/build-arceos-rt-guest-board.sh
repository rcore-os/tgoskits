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
BOARD="${BOARD:-orangepi-5-plus}"
IMAGE_DIR="${AXVISOR_ROOT}/images/orangepi_5_plus_arceos_rt"
OUTPUT_NAME="orangepi-5-plus-rt-latency"
APP_FEATURES="${RT_LATENCY_FEATURES:-rt-latency,rt-latency-guest,rt-latency-stress-short}"

info() { echo "[task1-board] $*"; }

guest_build_config() {
  local features=",${1},"
  if [[ "${features}" == *",rt-latency-stress-short,"* ]]; then
    echo "test-suit/arceos/rust/build-aarch64-rt-latency-board-orangepi.toml"
  else
    echo "test-suit/arceos/rust/build-aarch64-rt-latency-board-orangepi-smoke.toml"
  fi
}

CONFIG="$(guest_build_config "${APP_FEATURES}")"
ELF="${TGOSKITS_ROOT}/target/aarch64-unknown-linux-musl/release/arceos-test-suit"

info "Building rt-latency board guest via cargo xtask (${BOARD}, APP_FEATURES=${APP_FEATURES})..."
cd "${TGOSKITS_ROOT}"
cargo xtask arceos build --arch aarch64 --package arceos-test-suit --config "${CONFIG}"

if [[ ! -f "${ELF}" ]]; then
  echo "rt-latency guest ELF not found at ${ELF}" >&2
  exit 1
fi

mkdir -p "${IMAGE_DIR}"
rust-objcopy --strip-all -O binary "${ELF}" "${IMAGE_DIR}/${OUTPUT_NAME}"

info "Installed ${IMAGE_DIR}/${OUTPUT_NAME} ($(stat -c '%s' "${IMAGE_DIR}/${OUTPUT_NAME}") bytes)"
info "Deploy: ./scripts/task1/deploy-board-rt-guest.sh [board-ip]"
info "VM configs: configs/vms/orangepi-5-plus/arceos-rt-smp1*.toml (image_location=fs)"
