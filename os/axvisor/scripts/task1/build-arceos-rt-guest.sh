#!/usr/bin/env bash
# Build ArceOS rt-latency guest image for AxVisor memory-loaded RT VM.
#
# Uses the bare-metal ArceOS make path (aarch64-unknown-none-softfloat). The
# cargo xtask / musl PIE path produces a PIE userspace image that page-faults
# when loaded as a flat kernel at 0x8020_0000.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
AXVISOR_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
TGOSKITS_ROOT="$(cd "${AXVISOR_ROOT}/../.." && pwd)"
ARCEOS_ROOT="${TGOSKITS_ROOT}/os/arceos"
APP_DIR="${TGOSKITS_ROOT}/test-suit/arceos/rust"
IMAGE_DIR="${AXVISOR_ROOT}/images/qemu_aarch64_arceos_rt"
OUTPUT_NAME="qemu-aarch64-rt-latency-bench"
APP_FEATURES="${RT_LATENCY_FEATURES:-rt-latency,rt-latency-guest}"

info() { echo "[task1] $*"; }

info "Building bare-metal ArceOS rt-latency guest (APP_FEATURES=${APP_FEATURES})..."
cd "${ARCEOS_ROOT}"
make A="${APP_DIR}" ARCH=aarch64 SMP=1 LOG=info APP_FEATURES="${APP_FEATURES}"

BIN_SRC="${APP_DIR}/rust_aarch64.bin"
if [[ ! -f "${BIN_SRC}" ]]; then
  echo "rt-latency guest binary not found at ${BIN_SRC}" >&2
  exit 1
fi

mkdir -p "${IMAGE_DIR}"
install -m 0644 "${BIN_SRC}" "${IMAGE_DIR}/${OUTPUT_NAME}"

# Keep manual setup (`tmp/configs` + `tmp/images`) in sync when present.
TMP_IMAGE_DIR="${AXVISOR_ROOT}/tmp/images/qemu_aarch64_arceos_rt"
mkdir -p "${TMP_IMAGE_DIR}"
install -m 0644 "${BIN_SRC}" "${TMP_IMAGE_DIR}/${OUTPUT_NAME}"

info "Installed ${IMAGE_DIR}/${OUTPUT_NAME}"
info "Also synced ${TMP_IMAGE_DIR}/${OUTPUT_NAME}"
info "Use with configs/vms/qemu/aarch64/arceos-rt-smp1.toml (image_location = memory)"
info "Long-run guest: RT_LATENCY_FEATURES=rt-latency,rt-latency-guest,rt-latency-long $0"
