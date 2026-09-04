#!/usr/bin/env bash
# Run ArceOS RT guest rt-latency short benchmark under AxVisor (pCPU3, post-opt).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${ROOT}"

echo "[task1] Building rt-latency guest image (short, mode=guest)..."
RT_LATENCY_FEATURES=rt-latency,rt-latency-guest \
  "${ROOT}/os/axvisor/scripts/task1/build-arceos-rt-guest.sh"

echo "[task1] Running ArceOS RT guest rt-latency under AxVisor..."
cargo xtask axvisor test qemu --arch aarch64 -c arceos-rt-latency-guest
