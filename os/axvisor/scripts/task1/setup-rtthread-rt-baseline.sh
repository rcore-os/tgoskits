#!/usr/bin/env bash
# Prepare RT-Thread QEMU guest image for Task 1 RT baseline (pCPU3).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
AXVISOR_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
TGOSKITS_ROOT="$(cd "${AXVISOR_ROOT}/../.." && pwd)"
IMAGE_DIR="${AXVISOR_ROOT}/images/qemu_aarch64_rtthread"
IMAGE_NAME="qemu-aarch64-rtthread-bench"

info() { echo "[task1] $*"; }

if [[ ! -f "${IMAGE_DIR}/${IMAGE_NAME}" ]]; then
  info "Local RT-Thread image missing; building..."
  "${SCRIPT_DIR}/build-rtthread-rt-guest.sh"
fi

if [[ ! -f "${IMAGE_DIR}/${IMAGE_NAME}" ]]; then
  echo "RT-Thread guest image not found at ${IMAGE_DIR}/${IMAGE_NAME}" >&2
  exit 1
fi

info "RT-Thread image ready: ${IMAGE_DIR}/${IMAGE_NAME}"
info "VM config: configs/vms/qemu/aarch64/rtthread-rt-baseline.toml (phys_cpu_ids = [3])"
info "Smoke test: cargo xtask axvisor test qemu --arch aarch64 -c rtthread-rt-baseline"
