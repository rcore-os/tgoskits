#!/usr/bin/env bash
# Prepare pulled Zephyr QEMU guest image for Task 1 RT baseline (pCPU3).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
AXVISOR_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
TGOSKITS_ROOT="$(cd "${AXVISOR_ROOT}/../.." && pwd)"
IMAGE_DIR="${AXVISOR_ROOT}/images/qemu-aarch64/zephyr"
IMAGE_NAME="zephyr-qemu"

info() { echo "[task1] $*"; }

info "Pulling Zephyr QEMU guest image (qemu-aarch64 bundle)..."
(
  cd "${TGOSKITS_ROOT}"
  cargo xtask image pull qemu-aarch64 --output-dir "${AXVISOR_ROOT}/images"
)

if [[ ! -f "${IMAGE_DIR}/${IMAGE_NAME}" ]]; then
  echo "Zephyr guest image not found at ${IMAGE_DIR}/${IMAGE_NAME}" >&2
  exit 1
fi

info "Zephyr image ready: ${IMAGE_DIR}/${IMAGE_NAME}"
info "VM config: configs/vms/qemu/aarch64/zephyr-rt-baseline.toml (phys_cpu_ids = [3])"
info "Smoke test: cargo xtask axvisor test qemu --arch aarch64 -c zephyr-rt-baseline"
