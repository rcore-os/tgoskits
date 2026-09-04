#!/usr/bin/env bash
# Build ArceOS rt-latency guest image for AxVisor memory-loaded RT VM.
#
# arceos-test-suit enables `ax-std/arceos` (std-compat). The ArceOS Makefile
# compiles that with `-Z build-std=core,alloc`, which fails with ax-std/libc
# type errors and a missing panic handler. Reuse the same `cargo xtask arceos`
# musl std path as the passing native rt-latency baseline, then rust-objcopy a
# flat binary for `image_location = "memory"` at 0x8020_0000.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
AXVISOR_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
TGOSKITS_ROOT="$(cd "${AXVISOR_ROOT}/../.." && pwd)"
IMAGE_DIR="${AXVISOR_ROOT}/images/qemu_aarch64_arceos_rt"
OUTPUT_NAME="qemu-aarch64-rt-latency-bench"
APP_FEATURES="${RT_LATENCY_FEATURES:-rt-latency,rt-latency-guest}"

info() { echo "[task1] $*"; }

guest_build_config() {
  local features=",${1},"
  if [[ "${features}" == *",rt-latency-long,"* ]]; then
    echo "test-suit/arceos/rust/build-aarch64-rt-latency-long-guest.toml"
  elif [[ "${features}" == *",rt-latency-stress-short,"* ]]; then
    echo "test-suit/arceos/rust/build-aarch64-rt-latency-stress-short-guest.toml"
  else
    echo "test-suit/arceos/rust/build-aarch64-rt-latency-guest.toml"
  fi
}

CONFIG="$(guest_build_config "${APP_FEATURES}")"
ELF="${TGOSKITS_ROOT}/target/aarch64-unknown-linux-musl/release/arceos-test-suit"

info "Building rt-latency guest via cargo xtask (APP_FEATURES=${APP_FEATURES}, config=${CONFIG})..."
cd "${TGOSKITS_ROOT}"
cargo xtask arceos build --arch aarch64 --package arceos-test-suit --config "${CONFIG}"

if [[ ! -f "${ELF}" ]]; then
  echo "rt-latency guest ELF not found at ${ELF}" >&2
  exit 1
fi

mkdir -p "${IMAGE_DIR}"
rust-objcopy --strip-all -O binary "${ELF}" "${IMAGE_DIR}/${OUTPUT_NAME}"

TMP_IMAGE_DIR="${AXVISOR_ROOT}/tmp/images/qemu_aarch64_arceos_rt"
mkdir -p "${TMP_IMAGE_DIR}"
install -m 0644 "${IMAGE_DIR}/${OUTPUT_NAME}" "${TMP_IMAGE_DIR}/${OUTPUT_NAME}"

info "Installed ${IMAGE_DIR}/${OUTPUT_NAME}"
info "Also synced ${TMP_IMAGE_DIR}/${OUTPUT_NAME}"
info "Use with configs/vms/qemu/aarch64/arceos-rt-smp1.toml (image_location = memory)"
info "Long-run guest: RT_LATENCY_FEATURES=rt-latency,rt-latency-guest,rt-latency-long $0"
