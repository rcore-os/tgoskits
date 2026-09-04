#!/usr/bin/env bash
# Launch AxVisor with Linux 2-vCPU guest (Task 1).
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${REPO_ROOT}"

if [[ ! -f tmp/configs/linux-aarch64-qemu-smp2.toml ]]; then
  ./scripts/task1/setup-qemu-aarch64.sh
fi

exec cargo xtask qemu \
  --config "$(pwd)/tmp/configs/qemu-aarch64.toml" \
  --qemu-config "$(pwd)/tmp/configs/qemu-aarch64-runtime.toml" \
  --vmconfigs "$(pwd)/tmp/configs/linux-aarch64-qemu-smp2.toml"
