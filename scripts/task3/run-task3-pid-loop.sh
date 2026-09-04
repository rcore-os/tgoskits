#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${ROOT}"
./scripts/task2/setup-peer-initramfs.sh
./scripts/task3/setup-ai-loop.sh
exec cargo xtask axvisor test qemu --arch aarch64 -c task3-pid-loop "$@"
