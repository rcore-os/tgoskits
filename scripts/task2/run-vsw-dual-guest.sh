#!/usr/bin/env bash
# Run Task 2 dual-guest vsw smoke (ping 10.0.9.2 ↔ 10.0.9.3).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${ROOT}"
./scripts/task2/setup-peer-initramfs.sh
./scripts/task2/setup-udp-probe.sh
./scripts/task2/setup-icpc-smoke.sh
exec cargo xtask axvisor test qemu --arch aarch64 -c vsw-dual-guest "$@"
