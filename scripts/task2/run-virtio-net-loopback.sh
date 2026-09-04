#!/usr/bin/env bash
# Task 2 phase-1: VirtioNet loopback smoke under AxVisor.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${ROOT}"

echo "[task2] Running virtio-net-loopback smoke..."
cargo xtask axvisor test qemu --arch aarch64 -c virtio-net-loopback
echo "[task2] virtio-net-loopback OK"
