#!/usr/bin/env bash
#
# self-compile.sh — Boot StarryOS, compile itself inside QEMU, save the binary.
#
# Prerequisites:
#   - scripts/prepare-selfhost-rootfs.sh (run once to create the selfhost rootfs)
#   - qemu-system-<arch>, expect (for console automation)
#
# Usage:
#   ./scripts/self-compile.sh [OPTIONS] [rootfs-image]
#
#   --arch <arch>   Target architecture: riscv64 (default), x86_64, aarch64.
#   --smp <N>       Number of QEMU CPUs and cargo build jobs (default: 4).
#   --jobs <N>      Cargo build jobs (default: same as --smp).
#   rootfs-image    Path to the selfhost rootfs image (by arch default).
#
#   x86_64 notes: KVM acceleration is enabled for ~10x faster compilation.
#                 Requires /dev/kvm access and host x86_64 CPU.
#
# Output:
#   Saves the self-compiled starryos binary to /opt/starryos-selfbuilt inside the
#   rootfs image. The next script (run-selfbuilt-kernel.sh) extracts and boots it.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

info()  { printf "[self-compile] %s\n" "$*"; }
error() { printf "[self-compile] ERROR: %s\n" "$*" >&2; exit 1; }

# ─── Argument parsing ───────────────────────────────────────────────────────────

ARCH="riscv64"
SMP=4
CARGO_BUILD_JOBS=""
ROOTFS_IMG=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --arch) ARCH="$2"; shift 2 ;;
        --smp)  SMP="$2"; shift 2 ;;
        --jobs) CARGO_BUILD_JOBS="$2"; shift 2 ;;
        --help|-h)
            echo "Usage: $0 [--arch riscv64|x86_64|aarch64] [--smp N] [rootfs-image]"
            exit 0
            ;;
        *) ROOTFS_IMG="$1"; shift ;;
    esac
done

# ─── Architecture mapping ───────────────────────────────────────────────────────

case "$ARCH" in
    riscv64)
        TARGET="riscv64gc-unknown-none-elf"
        QEMU_BIN="qemu-system-riscv64"
        QEMU_MACHINE="virt"
        QEMU_CPU="rv64"
        QEMU_EXTRA=""  # extra flags appended after -cpu
        QEMU_BLK_DEV="virtio-blk-pci,drive=disk0"
        QEMU_NET_DEV="virtio-net-pci,netdev=net0"
        ;;
    x86_64)
        TARGET="x86_64-unknown-none"
        QEMU_BIN="qemu-system-x86_64"
        QEMU_MACHINE="q35"
        if [ -r /dev/kvm ] && [ -w /dev/kvm ]; then
            QEMU_CPU="host"
            QEMU_EXTRA="-enable-kvm"
        else
            QEMU_CPU="IvyBridge"
            QEMU_EXTRA=""
            info "KVM not available — using TCG emulation (will be slow)"
        fi
        QEMU_BLK_DEV="virtio-blk-pci,drive=disk0"
        QEMU_NET_DEV="virtio-net-pci,netdev=net0"
        ;;
    aarch64)
        TARGET="aarch64-unknown-none-softfloat"
        QEMU_BIN="qemu-system-aarch64"
        QEMU_MACHINE="virt"
        QEMU_CPU="cortex-a72"
        QEMU_EXTRA=""
        QEMU_BLK_DEV="virtio-blk-device,drive=disk0"
        QEMU_NET_DEV="virtio-net-device,netdev=net0"
        ;;
    *)
        error "Unsupported arch: $ARCH (valid: riscv64, x86_64, aarch64)"
        ;;
esac

: "${CARGO_BUILD_JOBS:=$SMP}"

# Default rootfs image per arch
if [ -z "$ROOTFS_IMG" ]; then
    case "$ARCH" in
        riscv64)  ROOTFS_IMG="tmp/axbuild/rootfs/rootfs-riscv64-debian-selfhost-v2.img" ;;
        x86_64)   ROOTFS_IMG="tmp/axbuild/rootfs/rootfs-x86_64-debian-selfhost.img" ;;
        aarch64)  ROOTFS_IMG="tmp/axbuild/rootfs/rootfs-aarch64-debian-selfhost.img" ;;
    esac
fi

QEMU_LOG="/tmp/starryos-selfcompile-$$.log"

info "Architecture: $ARCH | Target: $TARGET | SMP: $SMP | Cargo jobs: $CARGO_BUILD_JOBS"

# ─── Prerequisite checks ───────────────────────────────────────────────────────

for cmd in "$QEMU_BIN" expect debugfs; do
    command -v "$cmd" &>/dev/null || error "$cmd not found"
done

sudo -n true 2>/dev/null || error "passwordless sudo required for loopback mount"

[ -f "$ROOTFS_IMG" ] || error "Rootfs image not found: $ROOTFS_IMG (use --arch to select an architecture with a prepared rootfs)"
[ -f "$REPO_ROOT/Cargo.toml" ] || error "Not at repo root"

# ─── Step 1: Build seed kernel ─────────────────────────────────────────────────

info "Building seed kernel for $ARCH..."
cargo xtask starry build --arch "$ARCH" || error "Seed kernel build failed"

# Try release first (xtask builds --release by default), fall back to debug
SEED_KERNEL="$REPO_ROOT/target/${TARGET}/release/starryos"
if [ ! -f "$SEED_KERNEL" ]; then
    SEED_KERNEL="$REPO_ROOT/target/${TARGET}/debug/starryos"
fi
[ -f "$SEED_KERNEL" ] || error "Seed kernel not found: tried release and debug paths"
info "Seed kernel: $SEED_KERNEL (target: $TARGET)"

# ─── Step 2: Inject files into rootfs via loopback mount ───────────────────
# Uses loopback mount instead of debugfs -w because debugfs writes raw blocks
# without proper directory entry updates, corrupting the filesystem.

# Generate the inner self-compile script first
INNER_SCRIPT="$(mktemp /tmp/self-compile-inner.XXXXXX)"
cat > "$INNER_SCRIPT" << INNER_EOF
#!/usr/bin/bash
set -euo pipefail

export CARGO_TARGET_DIR=/tmp/build
mkdir -p /tmp/build 2>/dev/null || true
export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS}
export PATH=/root/.cargo/bin:/usr/local/bin:/usr/bin:/bin
export RUSTUP_HOME=/root/.rustup
export CARGO_HOME=/root/.cargo

cd /opt/starryos

# # Pre-extracted registry sources from nspawn are readable by
# rsext4 — cargo only needs READ access during compilation.
# Registry crate dep-graph writes go to tmpfs to work around an
# ext4 cache coherence bug (directory modifications via write_blocks
# may not be visible to subsequent lookups via read_blocks).
echo "[self-compile] Moving registry src to tmpfs..."
REGISTRY_SRC=/root/.cargo/registry/src
if ! mountpoint -q "\$REGISTRY_SRC" 2>/dev/null; then
    mkdir -p /tmp/.registry-src
    cp -a "\$REGISTRY_SRC"/. /tmp/.registry-src/
    mount -t tmpfs -o size=1500M none "\$REGISTRY_SRC"
    cp -a /tmp/.registry-src/. "\$REGISTRY_SRC"/
    rm -rf /tmp/.registry-src
fi
# Local workspace crate dep-graph writes go to a tiny tmpfs too.
mkdir -p /opt/starryos/false
if ! mountpoint -q /opt/starryos/false 2>/dev/null; then
    mount -t tmpfs -o size=100M none /opt/starryos/false
fi
echo "[self-compile] tmpfs setup complete"

echo "[self-compile] ARG ARCH=${ARCH} TARGET=${TARGET} SMP=${SMP} CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS}"

	# Filter workspace for arch-specific crates.
	echo "[self-compile] Filtering workspace for ${ARCH}..."
	/usr/bin/filter-workspace.sh "${ARCH}" Cargo.toml

# Patch axalloc to 64G capacity
echo "[self-compile] Patching page allocator to 64G..."
if [ -f os/arceos/modules/axalloc/Cargo.toml ] && [ -s os/arceos/modules/axalloc/Cargo.toml ]; then
    sed -i '/^default = /s|page-alloc-4g|page-alloc-64g|g' os/arceos/modules/axalloc/Cargo.toml
fi

export RUSTFLAGS="-Ccodegen-units=16 -Copt-level=0 -Cincremental=false -Clink-arg=-Tlinker.x -Clink-arg=-no-pie -Clink-arg=-znostart-stop-gc"
export AX_CONFIG_PATH=/opt/starryos/.axconfig.toml
echo "[self-compile] AX_CONFIG_PATH=\$AX_CONFIG_PATH"
echo "[self-compile] Rustc version: \$(rustc --version 2>/dev/null || echo 'unknown')"
echo "[self-compile] Cargo version: \$(cargo --version 2>/dev/null || echo 'unknown')"
echo "[self-compile] Starting cargo build (target=${TARGET}, jobs=\$CARGO_BUILD_JOBS)..."
echo "BUILD_START"

		export CARGO_TERM_PROGRESS_WHEN=always
		export CARGO_TERM_PROGRESS_WIDTH=120
		set +e
		export PATH=/root/.cargo/bin:/usr/bin:/bin; /usr/bin/bash -c 'while true; do sleep 30; echo "[self-compile] ... still compiling ..."; done' &
		HEARTBEAT_PID=\$!
		# Direct output to serial console (TTY = line-buffered cargo)
		cargo build --ignore-rust-version -p starryos \
		            --target ${TARGET} \
		            --features qemu,ax-driver/virtio-blk,ax-driver/virtio-net,ax-driver/virtio-gpu,ax-driver/virtio-input,ax-driver/virtio-socket \
		            --offline
		BUILD_RC=\$?
		kill \$HEARTBEAT_PID 2>/dev/null || true
		wait \$HEARTBEAT_PID 2>/dev/null || true
		echo "BUILD_END"
# filter-workspace.sh creates Cargo.toml.bak; restore for cleanliness
[ -f Cargo.toml.bak ] && mv Cargo.toml.bak Cargo.toml

BINARY=/tmp/build/${TARGET}/debug/starryos
if [ \$BUILD_RC -eq 0 ] && [ -f "\$BINARY" ] && [ -s "\$BINARY" ]; then
    cp "\$BINARY" /opt/starryos-selfbuilt
    sync
    echo "BINARY_SIZE=\$(stat -c%s "\$BINARY")"
    echo "SELF_COMPILE_SUCCESS"
else
    echo "SELF_COMPILE_FAILED: rc=\$BUILD_RC binary=\$BINARY"
fi
INNER_EOF
# Mount rootfs via loopback for reliable file injection (kernel ext4 driver)
info "Mounting rootfs via loopback for file injection..."
MNT_DIR="$(mktemp -d /tmp/rootfs-mount.XXXXXX)"
MNT_LOOP="$(sudo losetup -f --show "$ROOTFS_IMG")"
cleanup_mount() {
    sudo umount "$MNT_DIR" 2>/dev/null || true
    sudo losetup -d "$MNT_LOOP" 2>/dev/null || true
    rmdir "$MNT_DIR" 2>/dev/null || true
}
trap cleanup_mount EXIT
sudo mount "$MNT_LOOP" "$MNT_DIR"

# Inject linker.x (generated by seed kernel build)
KERNEL_DIR2=$(dirname "$SEED_KERNEL")
LINKER_X="$KERNEL_DIR2/linker.x"
if [ -n "$LINKER_X" ] && [ -f "$LINKER_X" ]; then
    sudo cp "$LINKER_X" "$MNT_DIR/opt/starryos/linker.x"
    info "linker.x injected"
fi

# Inject .axconfig.toml
HOST_AXCONFIG="$REPO_ROOT/tmp/axbuild/axconfig/starryos/${TARGET}/.axconfig.toml"
if [ -f "$HOST_AXCONFIG" ]; then
    sudo cp "$HOST_AXCONFIG" "$MNT_DIR/opt/starryos/.axconfig.toml"
    info ".axconfig.toml injected"
fi

# Inject axalloc Cargo.toml (may have been removed by e2fsck on prior runs)
HOST_AXALLOC_CARGO="$REPO_ROOT/os/arceos/modules/axalloc/Cargo.toml"
if [ -f "$HOST_AXALLOC_CARGO" ]; then
    sudo cp "$HOST_AXALLOC_CARGO" "$MNT_DIR/opt/starryos/os/arceos/modules/axalloc/Cargo.toml"
    info "axalloc Cargo.toml injected"
fi

# Inject filter-workspace.sh (deduplicated arch-members filtering)
sudo cp "$REPO_ROOT/scripts/filter-workspace.sh" "$MNT_DIR/usr/bin/filter-workspace.sh"
sudo chmod +x "$MNT_DIR/usr/bin/filter-workspace.sh"
info "filter-workspace.sh injected"

# Inject inner compile script (the only file that changes between runs)
sudo mkdir -p "$MNT_DIR/usr/bin"
sudo cp "$INNER_SCRIPT" "$MNT_DIR/usr/bin/self-compile-inner.sh"
sudo chmod +x "$MNT_DIR/usr/bin/self-compile-inner.sh"
rm -f "$INNER_SCRIPT"
info "Compile script injected at /usr/bin/self-compile-inner.sh"

# Ensure /run/udev/data exists to prevent init EIO errors
sudo mkdir -p "$MNT_DIR/run/udev/data"

# Sync before unmount to flush all writes to disk
sync
sudo umount "$MNT_DIR"
sudo losetup -d "$MNT_LOOP"
rmdir "$MNT_DIR"

# ─── Step 3: Boot QEMU and run the compile via expect ──────────────────────────

info "Booting QEMU ($QEMU_BIN) for self-compilation (this may take ~2 hours)..."
info "QEMU log: $QEMU_LOG"

set +e
expect << EXPECT_EOF 2>&1 | tee "$QEMU_LOG"
set timeout 7500
log_user 1

spawn $QEMU_BIN \
    -nographic \
    -machine $QEMU_MACHINE \
    -cpu $QEMU_CPU \
    $QEMU_EXTRA \
    -smp $SMP \
    -m 16G \
    -kernel $SEED_KERNEL \
    -device $QEMU_BLK_DEV \
    -drive id=disk0,if=none,format=raw,cache=writeback,file=$ROOTFS_IMG,file.locking=off \
    -device $QEMU_NET_DEV \
    -netdev user,id=net0

# Wait for the StarryOS shell prompt
expect {
    -re {root@starry[:~]} { }
    -re {starry:/[\$#] } { }
    timeout { puts "TIMEOUT waiting for shell prompt"; exit 1 }
        eof {
            puts "QEMU_EXITED_EARLY"
            exit 1
        }
}

# Launch the inner compile script
send -- "/usr/bin/self-compile-inner.sh\r"
expect {
    -re {SELF_COMPILE_SUCCESS} {
        puts "COMPILE_OK"
    }
    -re {SELF_COMPILE_FAILED[^\r\n]*} {
        puts "COMPILE_FAILED"
        exit 1
    }
    timeout {
        puts "TIMEOUT during compilation"
        exit 1
    }
}

# Quit QEMU via the QEMU monitor (Ctrl+A c quit)
send -- "\x01c"
sleep 1
send -- "quit\r"
expect {
    timeout { puts "SHUTDOWN_TIMEOUT"; exit 0 }
    eof { puts "QEMU_EXITED" }
}
EXPECT_EOF

EXPECT_EXIT=$?
set -e

# ─── Step 4: Verify result ─────────────────────────────────────────────────────

if [ "$EXPECT_EXIT" -eq 0 ]; then
    # Verify the binary was saved inside the rootfs
    BINARY_SIZE=$(debugfs -R "stat /opt/starryos-selfbuilt" "$ROOTFS_IMG" 2>/dev/null | grep -oP 'Size: \K[0-9]+' | head -1 || echo "0")
    if [ "$BINARY_SIZE" -gt 1000000 ]; then
        info "Self-compilation SUCCESS — binary saved to /opt/starryos-selfbuilt (${BINARY_SIZE} bytes)"
        info "Run ./scripts/run-selfbuilt-kernel.sh to boot with this kernel"
    else
        error "Binary verification failed (size=$BINARY_SIZE)"
    fi
else
    error "Self-compilation FAILED. Check QEMU log: $QEMU_LOG"
fi
