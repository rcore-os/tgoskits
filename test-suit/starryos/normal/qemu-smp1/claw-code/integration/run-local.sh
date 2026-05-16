#!/usr/bin/env bash
# Download claw binary from GitHub, inject into rootfs, and boot StarryOS.
# Usage: docker run --rm -v "$(pwd)":/workspace -w /workspace starryos-dev:ubuntu-qemu10.2.1 \
#   bash test-suit/starryos/normal/qemu-smp1/claw-code/integration/run-local.sh
set -eu

CLAW_URL="https://github.com/MuZhao2333/tgoskits/releases/download/claw-code-binary/claw"
ROOTFS="/workspace/tmp/axbuild/rootfs/rootfs-x86_64-alpine.img"
CLAW_BIN="/tmp/claw"

echo "=== 1. Build StarryOS (ensures rootfs exists) ==="
cargo xtask starry quick-start qemu-x86_64 build

echo "=== 2. Download claw binary ==="
if [ ! -f "$CLAW_BIN" ]; then
    curl -sL -o "$CLAW_BIN" "$CLAW_URL"
    chmod +x "$CLAW_BIN"
    echo "Downloaded: $CLAW_BIN ($(du -h "$CLAW_BIN" | cut -f1))"
else
    echo "Already downloaded: $CLAW_BIN"
fi

echo "=== 3. Inject claw into rootfs ==="
debugfs -w "$ROOTFS" -R "rm /usr/bin/claw" 2>/dev/null || true
debugfs -w "$ROOTFS" -R "write $CLAW_BIN /usr/bin/claw"
debugfs -w "$ROOTFS" -R "sif /usr/bin/claw mode 0100755"
echo "Injected claw into rootfs"

echo "=== 4. Boot StarryOS ==="
cargo xtask starry quick-start qemu-x86_64 run
