#!/usr/bin/env bash
# Build claw from source, inject into rootfs, and boot StarryOS.
# Usage: docker run --rm -v "$(pwd)":/workspace -w /workspace starryos-dev:ubuntu-qemu10.2.1 \
#   bash test-suit/starryos/normal/qemu-smp1/claw-code/integration/run-local.sh
set -eu

BUILD_SCRIPT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/build-claw.sh"
ROOTFS="/workspace/tmp/axbuild/rootfs/rootfs-x86_64-alpine.img"

echo "=== 1. Build StarryOS (ensures rootfs exists) ==="
cargo xtask starry quick-start qemu-x86_64 build

echo "=== 2. Build claw from source ==="
CLAW_BIN="$(bash "$BUILD_SCRIPT")"
echo "claw binary: $CLAW_BIN"

echo "=== 3. Inject claw into rootfs ==="
debugfs -w "$ROOTFS" -R "rm /usr/bin/claw" 2>/dev/null || true
debugfs -w "$ROOTFS" -R "write $CLAW_BIN /usr/bin/claw"
debugfs -w "$ROOTFS" -R "sif /usr/bin/claw mode 0100755"
echo "Injected claw into rootfs"

echo "=== 4. Boot StarryOS ==="
cargo xtask starry quick-start qemu-x86_64 run
