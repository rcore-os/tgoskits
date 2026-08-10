#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${ROOT}"
./scripts/task2/setup-peer-initramfs.sh
./scripts/task2/setup-icpc-reliability.sh
exec cargo xtask axvisor test qemu --arch aarch64 -g stress -c icpc-fault-inject "$@"
