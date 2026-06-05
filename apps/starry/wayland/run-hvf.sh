#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE="$(cd "$SCRIPT_DIR/../../.." && pwd)"
ARCH="aarch64"
TARGET="aarch64-unknown-none-softfloat"

ROOTFS_BASE="$WORKSPACE/tmp/axbuild/rootfs/rootfs-${ARCH}-alpine.img"
ROOTFS_APP="$WORKSPACE/tmp/axbuild/rootfs/rootfs-${ARCH}-wayland.img"
KERNEL="$WORKSPACE/target/${TARGET}/release/starryos.bin"
BUILD_CONFIG="$SCRIPT_DIR/build-${TARGET}.toml"
PROVISION_MARKER=".wayland-provisioned"

NO_BUILD=false
REPROVISION=false
USE_HVF=false

for arg in "$@"; do
    case "$arg" in
        --no-build)    NO_BUILD=true ;;
        --reprovision) REPROVISION=true ;;
        --hvf)         USE_HVF=true ;;
        *) echo "unknown: $arg" >&2; exit 1 ;;
    esac
done

DBSBIN="/opt/homebrew/opt/e2fsprogs/sbin"
export PATH="$DBSBIN:$PATH"

# ---- Build ----
if [ "$NO_BUILD" = false ]; then
    echo "==> Building StarryOS for $ARCH..."
    cd "$WORKSPACE"
    cargo xtask starry build --arch "$ARCH" --config "$BUILD_CONFIG"
fi
[ -f "$KERNEL" ] || { echo "error: kernel not found at $KERNEL" >&2; exit 1; }

# ---- Rootfs ----
mkdir -p "$(dirname "$ROOTFS_APP")"

if [ ! -f "$ROOTFS_BASE" ]; then
    echo "==> Downloading rootfs..."
    cd "$WORKSPACE"
    cargo xtask starry rootfs --arch "$ARCH"
fi

if [ ! -f "$ROOTFS_APP" ] || [ "$REPROVISION" = true ]; then
    echo "==> Creating wayland rootfs from base..."
    cp "$ROOTFS_BASE" "$ROOTFS_APP"
    chmod 0644 "$ROOTFS_APP"
    # Ensure QEMU user-mode networking DNS
    echo "nameserver 10.0.2.3" | \
        debugfs -w "$ROOTFS_APP" -R "cd /etc; rm resolv.conf; write /dev/stdin resolv.conf" 2>/dev/null || true
fi

# ---- Provision (first run only) ----
marker_exists() {
    debugfs -R "ls /$PROVISION_MARKER" "$ROOTFS_APP" >/dev/null 2>&1
}

if ! marker_exists || [ "$REPROVISION" = true ]; then
    echo "==> Provisioning: installing Weston + GTK4 demo (~3-5 min)..."

    # Write provision script into rootfs
    cat > /tmp/wayland-provision.sh <<'SCRIPT'
#!/bin/sh
echo "PROVISION_BEGIN"
apk add weston weston-backend-drm weston-shell-desktop gtk4.0-demo
echo "PROVISION_PACKAGES_DONE"
touch /.wayland-provisioned
echo "PROVISION_DONE"
poweroff -f
SCRIPT

    debugfs -w "$ROOTFS_APP" <<END
cd /usr/bin
rm provision-wayland.sh
write /tmp/wayland-provision.sh provision-wayland.sh
END

    # Headless boot: send the provision command once the shell is ready.
    # AArch64 TCG boot is slow — wait 25s before sending.
    echo "    (booting headless, aarch64 TCG takes ~20s to reach shell...)"
    (sleep 25; printf 'sh /usr/bin/provision-wayland.sh\n') | \
    timeout 900 qemu-system-aarch64 \
        -machine virt \
        -cpu cortex-a53 \
        -smp 4 -m 2048M \
        -nographic \
        -device virtio-blk-pci,drive=disk0 \
        -drive "id=disk0,if=none,format=raw,file=$ROOTFS_APP" \
        -device virtio-net-pci,netdev=net0 \
        -netdev user,id=net0 \
        -append "root=/dev/sda console=ttyS0" \
        -kernel "$KERNEL" 2>&1 | grep -E "PROVISION_|panic|Welcome|WAYLAND" | head -20

    # Verify marker was written
    if marker_exists; then
        echo "==> Provision complete."
    else
        echo "==> WARNING: provision may have failed (no marker)."
        echo "    Run 'apk add weston weston-backend-drm weston-shell-desktop gtk4.0-demo' manually."
    fi
fi

# ---- Launch ----
CPU="cortex-a53"
ACCEL="TCG"
if [ "$USE_HVF" = true ]; then
    CPU="max"
    ACCEL="HVF"
fi

echo ""
echo "==> Launching StarryOS ($ACCEL, Cocoa display)..."
echo "    Login as root, then:"
echo "      weston --backend=drm-backend.so --renderer=pixman &"
echo "      gtk4-demo"
echo ""

exec qemu-system-aarch64 \
    -machine virt \
    -cpu "$CPU" \
    $([ "$USE_HVF" = true ] && echo "-accel hvf") \
    -smp 4 -m 2048M \
    -display cocoa,show-cursor=on \
    -vnc ":${STARRY_VNC:-0}" \
    -device virtio-gpu-pci \
    -device virtio-keyboard-pci \
    -device virtio-mouse-pci \
    -device virtio-blk-pci,drive=disk0 \
    -drive "id=disk0,if=none,format=raw,file=$ROOTFS_APP" \
    -device virtio-net-pci,netdev=net0 \
    -netdev "user,id=net0,hostfwd=tcp::2222-:22" \
    -append "root=/dev/sda console=ttyS0" \
    -serial stdio \
    -kernel "$KERNEL"
