#!/usr/bin/env bash
set -euo pipefail

WORKSPACE="${STARRY_WORKSPACE:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)}"
ROOTFS_DIR="$WORKSPACE/target/axbuild/rootfs"
OVERLAY="${STARRY_OVERLAY_DIR:-$WORKSPACE/tmp/axbuild/starry-app/claw-code/overlay}"

echo "=== 1. Build claw from source ==="
BUILD_SCRIPT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../claw-code-regression" && pwd)/build-claw.sh"
CLAW_BIN="$(bash "$BUILD_SCRIPT")"

echo "=== 2. Prepare rootfs ==="
BASE_ROOTFS="${STARRY_BASE_ROOTFS:-$ROOTFS_DIR/rootfs-${STARRY_ARCH:-x86_64}-alpine.img}"
APP_ROOTFS="${STARRY_ROOTFS:-$ROOTFS_DIR/rootfs-${STARRY_ARCH:-x86_64}-claw-code.img}"
if [ "$BASE_ROOTFS" != "$APP_ROOTFS" ]; then
    mkdir -p "$(dirname "$APP_ROOTFS")"
    rm -f "$APP_ROOTFS"
    cp "$BASE_ROOTFS" "$APP_ROOTFS"
fi

echo "=== 3. Inject claw into rootfs ==="
inject_claw() {
    local img="$1"
    echo "  injecting into $img ..."
    debugfs -w "$img" -R "rm /usr/bin/claw" 2>/dev/null || true
    debugfs -w "$img" -R "write $CLAW_BIN /usr/bin/claw" >/dev/null
    debugfs -w "$img" -R "sif /usr/bin/claw mode 0100755"
    debugfs -w "$img" -R "rm /usr/bin/claw.provenance" 2>/dev/null || true
    debugfs -w "$img" -R "write $CLAW_BIN.provenance /usr/bin/claw.provenance" >/dev/null
}
inject_claw "$APP_ROOTFS"

echo "Injected claw into rootfs"

# Place a marker so the overlay is never empty (app framework requires it).
mkdir -p "$OVERLAY"
touch "${OVERLAY}/.claw-injected"
