#!/usr/bin/env bash
# StarryOS <-> FreeRTOS VTP demo runner (QEMU aarch64 under Axvisor).
#
# Phases:
#   1. Build the StarryOS guest kernel (cargo xtask starry build).
#   2. Build the Starry VTP server for aarch64 and inject it into the rootfs
#      image that the Starry guest mounts from the passed-through NVMe.
#   3. Ensure the FreeRTOS guest image exists (build/freertos.bin).
#   4. Run the E2E: cargo xtask axvisor test qemu --test-case vtp.
#
# Requires the project container or a Linux host with the tgoskits toolchain
# (QEMU, cross toolchains) — see docs/docs/build/ci.md.
#
# Known integration points (may need tuning per environment):
#   - the aarch64 cross compiler for the VTP server (aarch64-linux-gnu-gcc or
#     aarch64-linux-musl-gcc), detected automatically;
#   - the Starry rootfs device path passed through to VM1 (nvme), and
#   - the FreeRTOS image path (guests/freertos-vtp/build/freertos.bin).

set -euo pipefail

workspace=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)
cd "$workspace"

ARCH=aarch64
STARRY_BIN="target/aarch64-unknown-none-softfloat/release/starryos.bin"
FREERTOS_BIN="guests/freertos-vtp/build/freertos.bin"
ROOTFS_IMG="tmp/axbuild/rootfs/rootfs-${ARCH}-alpine.img"
VTP_SERVER_SRC="test-suit/axvisor/normal/qemu-vtp/protocol"
VTP_SERVER_APP="test-suit/starryos/qemu/system/axvisor-vtp-server/src/main.c"
STAGING=$(mktemp -d)
trap 'rm -rf "$STAGING"' EXIT

say() { printf '\n=== %s ===\n' "$*"; }

pick_cross_cc() {
    for cc in aarch64-linux-musl-gcc aarch64-linux-gnu-gcc aarch64-none-linux-gnu-gcc; do
        if command -v "$cc" >/dev/null 2>&1; then
            echo "$cc"
            return 0
        fi
    done
    return 1
}

# --- 1. Starry guest kernel -------------------------------------------------
say "Building StarryOS guest kernel ($ARCH)"
cargo xtask starry build --arch "$ARCH"

if [[ ! -f "$STARRY_BIN" ]]; then
    echo "ERROR: Starry image not produced at $STARRY_BIN" >&2
    exit 1
fi

# --- 2. Starry VTP server: build + inject into rootfs ----------------------
say "Building Starry VTP server (static aarch64)"
CC=$(pick_cross_cc || true)
if [[ -z "$CC" ]]; then
    echo "ERROR: no aarch64 cross C compiler found (need aarch64-linux-musl-gcc or -gnu-gcc)." >&2
    echo "Install it or build the Starry system test-suit to produce the binary." >&2
    exit 1
fi
echo "using compiler: $CC"
"$CC" -std=c11 -O2 -static -Wall -Wextra -Werror \
    -I"$VTP_SERVER_SRC" \
    "$VTP_SERVER_APP" "$VTP_SERVER_SRC/vtp.c" \
    -o "$STAGING/axvisor-vtp-server"

say "Ensuring Starry rootfs image is available"
cargo xtask starry rootfs --arch "$ARCH"

# Resolve the managed rootfs path (axbuild remaps the legacy path).
if [[ ! -f "$ROOTFS_IMG" ]]; then
    # try image storage location used by axbuild
    for cand in .tgos-images/rootfs-${ARCH}-alpine/rootfs-${ARCH}-alpine \
                .tgos-images/rootfs-${ARCH}-alpine/rootfs-${ARCH}-alpine.img; do
        if [[ -f "$cand" ]]; then ROOTFS_IMG="$cand"; break; fi
    done
fi
if [[ ! -f "$ROOTFS_IMG" ]]; then
    echo "ERROR: rootfs image not found ($ROOTFS_IMG). Run 'cargo xtask starry rootfs --arch $ARCH'." >&2
    exit 1
fi
echo "rootfs image: $ROOTFS_IMG"

say "Injecting axvisor-vtp-server into rootfs"
if command -v debugfs >/dev/null 2>&1; then
    debugfs -w -R "rm usr/bin/axvisor-vtp-server" "$ROOTFS_IMG" >/dev/null 2>&1 || true
    debugfs -w -R "mkdir usr/bin" "$ROOTFS_IMG" >/dev/null 2>&1 || true
    debugfs -w -R "write $STAGING/axvisor-vtp-server usr/bin/axvisor-vtp-server" "$ROOTFS_IMG"
else
    echo "debugfs not found; manually copy $STAGING/axvisor-vtp-server into the rootfs"
    echo "image at: $ROOTFS_IMG  (e.g. 'debugfs -w -R \"write <path> usr/bin/axvisor-vtp-server\" IMG')"
fi

# --- 3. FreeRTOS guest image ------------------------------------------------
say "Checking FreeRTOS guest image"
if [[ ! -f "$FREERTOS_BIN" ]]; then
    echo "ERROR: $FREERTOS_BIN missing." >&2
    echo "Build the FreeRTOS guest (guests/freertos-vtp) and place the linked image at:" >&2
    echo "  $FREERTOS_BIN" >&2
    echo "See guests/freertos-vtp/README.md for integration steps." >&2
    exit 1
fi

# --- 4. Run the E2E test ----------------------------------------------------
say "Running Axvisor QEMU VTP test case"
cargo xtask axvisor test qemu --arch "$ARCH" --test-group normal --test-case qemu-vtp
