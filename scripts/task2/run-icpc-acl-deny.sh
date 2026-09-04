#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${ROOT}"
./scripts/task2/setup-peer-initramfs.sh
./scripts/task2/setup-icpc-acl-deny.sh
exec cargo xtask axvisor test qemu --arch aarch64 -c icpc-acl-deny "$@"
