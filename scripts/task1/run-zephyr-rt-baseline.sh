#!/usr/bin/env bash
# Run Zephyr RT guest smoke under AxVisor (pCPU3, pulled qemu-aarch64 image).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${ROOT}"

echo "[task1] Ensuring Zephyr QEMU guest image..."
"${ROOT}/os/axvisor/scripts/task1/setup-zephyr-rt-baseline.sh"

echo "[task1] Running Zephyr RT baseline smoke under AxVisor..."
cargo xtask axvisor test qemu --arch aarch64 -c zephyr-rt-baseline
