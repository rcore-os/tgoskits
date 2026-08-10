#!/usr/bin/env bash
# Run bare-metal ArceOS RT latency baseline (Task 1 M3 native RTOS reference).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${ROOT}"

echo "[task1] Running ArceOS rt-latency bare-metal baseline on QEMU aarch64..."
cargo xtask arceos test qemu --arch aarch64 -g rust -c rt-latency
