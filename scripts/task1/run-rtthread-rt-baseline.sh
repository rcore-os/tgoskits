#!/usr/bin/env bash
# Run RT-Thread RT guest smoke under AxVisor (pCPU3).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${ROOT}"

echo "[task1] Ensuring RT-Thread QEMU guest image..."
"${ROOT}/os/axvisor/scripts/task1/setup-rtthread-rt-baseline.sh"

echo "[task1] Running RT-Thread RT baseline smoke under AxVisor..."
cargo xtask axvisor test qemu --arch aarch64 -c rtthread-rt-baseline
