#!/usr/bin/env bash
#
# prepare-selfhost-rootfs.sh — Maintainer tool to create a selfhost Debian rootfs
# blueprint with rustc, cargo, and the full StarryOS source tree for offline
# x86_64 self-compilation inside QEMU.
#
# This script requires sudo, debootstrap and systemd-nspawn.  It is NOT the
# primary verification path for reviewers; the produced blueprint image is
# reused by self-compile.sh (which clones it to a working copy each run).
#
# Usage:
#   sudo ./scripts/prepare-selfhost-rootfs.sh --arch riscv64|x86_64|aarch64 [--force]
#
#   --arch    Target architecture (required):
#               riscv64 — RISC-V 64-bit (needs existing Debian base image)
#               x86_64  — x86_64, native bootstrap, KVM-capable
#               aarch64 — AArch64 (ARM 64-bit), cross-arch bootstrap
#   --force   Overwrite existing output image.
#
# Prerequisites (auto-checked):
#   All:     debugfs, resize2fs, git, cargo, systemd-nspawn
#   riscv64: qemu-riscv64-static, base Debian riscv64 image
#   x86_64:  debootstrap (pacman -S debootstrap)
#   aarch64: qemu-aarch64-static, debootstrap (pacman -S debootstrap qemu-user-static-binfmt)
#
# Output:
#   riscv64  — tmp/axbuild/rootfs/rootfs-riscv64-debian-selfhost-v2.img
#   x86_64   — tmp/axbuild/rootfs/rootfs-x86_64-selfhost.img
#   aarch64  — tmp/axbuild/rootfs/rootfs-aarch64-debian-selfhost.img
#
# Example:
#   sudo ./scripts/prepare-selfhost-rootfs.sh --arch x86_64
#   ./scripts/self-compile.sh --arch x86_64
#   ./scripts/run-selfbuilt-kernel.sh --arch x86_64

set -euo pipefail

SCRIPT_NAME="$(basename "$0")"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

info()  { printf "[%s] %s\n" "$SCRIPT_NAME" "$*"; }
warn()  { printf "[%s] WARN: %s\n" "$SCRIPT_NAME" "$*" >&2; }
die()   { printf "[%s] ERROR: %s\n" "$SCRIPT_NAME" "$*" >&2; exit 1; }

# ─── Argument parsing ───────────────────────────────────────────────────────────

ARCH=""
FORCE=0
ORIGINAL_ARGS="$*"
while [[ $# -gt 0 ]]; do
    case "$1" in
        --arch) ARCH="$2"; shift 2 ;;
        --force) FORCE=1; shift ;;
        --help|-h)
            echo "Usage: $0 --arch riscv64|x86_64|aarch64 [--force]"
            echo ""
            echo "  riscv64 — RISC-V 64-bit (needs existing Debian base image)"
            echo "  x86_64  — x86_64 native bootstrap + KVM self-compilation"
            echo "  aarch64 — AArch64 cross-arch bootstrap"
            exit 0
            ;;
        *) die "Unknown option: $1" ;;
    esac
done
[ -z "$ARCH" ] && die "--arch is required (riscv64, x86_64, aarch64)"

# Normalize `aarch64` to `arm` (internal convention)
[ "$ARCH" = "aarch64" ] && ARCH="arm"

# ─── Architecture mapping ───────────────────────────────────────────────────────

ROOTFS_DIR="tmp/axbuild/rootfs"
mkdir -p "$ROOTFS_DIR"

case "$ARCH" in
    riscv64)
        TARGET="riscv64gc-unknown-none-elf"
        QEMU_STATIC="qemu-riscv64-static"
        NSPAWN_MACHINE="riscv64"
        DEBIAN_ARCH=""          # no debootstrap — use pre-built image
        BASE_IMG="$ROOTFS_DIR/rootfs-riscv64-debian.img"
        OUTPUT_IMG="$ROOTFS_DIR/rootfs-riscv64-debian-selfhost-v2.img"
        NEED_QEMU=1
        ;;
    x86_64)
        TARGET="x86_64-unknown-none"
        QEMU_STATIC=""
        NSPAWN_MACHINE=""
        DEBIAN_ARCH="amd64"
        BASE_IMG=""
        OUTPUT_IMG="$ROOTFS_DIR/rootfs-x86_64-selfhost.img"
        NEED_QEMU=0
        ;;
    arm)
        TARGET="aarch64-unknown-none-softfloat"
        QEMU_STATIC="qemu-aarch64-static"
        NSPAWN_MACHINE="arm64"
        DEBIAN_ARCH="arm64"
        BASE_IMG=""
        OUTPUT_IMG="$ROOTFS_DIR/rootfs-aarch64-debian-selfhost.img"
        NEED_QEMU=1
        ;;
    *)  die "Unsupported arch: $ARCH (valid: riscv64, x86_64, aarch64)" ;;
esac

DEST_PATH="/opt/starryos"
info "Arch: $ARCH | Target: $TARGET | Dest: $DEST_PATH"
[ "$NEED_QEMU" -eq 0 ] && info "Mode: native (no QEMU)" || info "Mode: cross-arch via $QEMU_STATIC"

# ─── Prerequisite checks ────────────────────────────────────────────────────────

# When invoked under sudo, the user's PATH (including ~/.cargo/bin) is
# stripped by sudo's secure_path.  Restore it from the invoking user's
# home so that `cargo` and other Rust toolchain binaries are findable.
if [ -n "${SUDO_USER:-}" ] && [ "$(id -u)" -eq 0 ]; then
    USER_HOME="$(eval echo ~$SUDO_USER)"
    export PATH="$USER_HOME/.cargo/bin:$PATH"
fi

info "Checking prerequisites..."
for cmd in debugfs resize2fs dd git cargo systemd-nspawn; do
    if ! command -v "$cmd" &>/dev/null; then
        if [ "$cmd" = "cargo" ] && [ -n "${SUDO_USER:-}" ]; then
            die "cargo not found in sudo PATH. Install rustup as the invoking user,"\
                " then retry with: sudo env PATH=\"\$PATH\" $0"\
                " ${ORIGINAL_ARGS:-}"
        fi
        die "$cmd not found"
    fi
done

if [ "$NEED_QEMU" -eq 1 ]; then
    command -v "$QEMU_STATIC" &>/dev/null || \
        die "$QEMU_STATIC not found. Install: sudo pacman -S qemu-user-static-binfmt"
fi

if [ -n "$DEBIAN_ARCH" ]; then
    command -v debootstrap &>/dev/null || \
        die "debootstrap not found. Install: sudo pacman -S debootstrap"
    [ "$(id -u)" -eq 0 ] || die "debootstrap requires root. Re-run with sudo."
fi

# ─── Idempotency check ──────────────────────────────────────────────────────────

if [ -f "$OUTPUT_IMG" ]; then
    if [ "$FORCE" -ne 1 ]; then
        warn "Output image exists: $OUTPUT_IMG"
        warn "Pass --force to overwrite."
        sleep 3
        die "Aborted. Use --force to overwrite."
    fi
    info "Removing existing output (--force)."
    rm -f "$OUTPUT_IMG"
fi

# ─── Helpers: run a command inside a rootfs image ───────────────────────────────

# Uses systemd-nspawn. Falls back to loopback mount + chroot if unavailable.
nspawn_run() {
    local img="$1"; shift
    local cmd="$*"
    local args=(--image="$img" --quiet)

    if [ "$NEED_QEMU" -eq 1 ]; then
        args+=(--bind="/usr/bin/$QEMU_STATIC")
        [ -n "$NSPAWN_MACHINE" ] && args+=(--machine="$NSPAWN_MACHINE")
    fi

    if command -v systemd-nspawn &>/dev/null; then
        systemd-nspawn "${args[@]}" /usr/bin/bash -c "$cmd"
    else
        warn "systemd-nspawn unavailable — using loopback chroot."
        _loopback_chroot "$img" "$cmd"
    fi
}

_loopback_chroot() {
    local img="$1" cmd="$2"
    local mnt="/mnt/selfhost-rootfs"
    mkdir -p "$mnt"

    local loop; loop="$(losetup -f --show "$img")"
    mount "$loop" "$mnt"

    local rc=0
    if [ "$NEED_QEMU" -eq 1 ]; then
        cp "/usr/bin/$QEMU_STATIC" "$mnt/usr/bin/"
        chroot "$mnt" "/usr/bin/$QEMU_STATIC" /usr/bin/bash -c "$cmd" || rc=$?
        rm -f "$mnt/usr/bin/$QEMU_STATIC"
    else
        chroot "$mnt" /usr/bin/bash -c "$cmd" || rc=$?
    fi

    umount "$mnt"
    losetup -d "$loop"

    if [ "$rc" -ne 0 ]; then
        warn "_loopback_chroot: command failed (exit code $rc)"
    fi
    return "$rc"
}

expand_image() {
    local img="$1" mb="$2"
    info "Expanding by ${mb} MiB..."
    dd if=/dev/zero bs=1M count="$mb" >> "$img" 2>/dev/null
    e2fsck -fy "$img" >/dev/null 2>&1 || true
    resize2fs "$img" >/dev/null 2>&1
    info "Size: $(($(stat --format=%s "$img") / 1024 / 1024)) MiB"
}

mount_image() {
    local img="$1" mnt="$2"
    mkdir -p "$mnt"
    local loop; loop="$(losetup -f --show "$img")"
    mount "$loop" "$mnt"
    echo "$loop"  # caller: LOOP=$(mount_image ...)
}

umount_image() {
    local mnt="$1" loop="$2"
    umount "$mnt" 2>/dev/null || true
    losetup -d "$loop" 2>/dev/null || true
}

# ═══════════════════════════════════════════════════════════════════════════════
# Step 1: Create base Debian rootfs
# ═══════════════════════════════════════════════════════════════════════════════

if [ -n "$DEBIAN_ARCH" ]; then
    # ── x86_64 / aarch64: create via debootstrap ────────────────────────────
    BOOTSTRAP_DIR="/tmp/debian-${DEBIAN_ARCH}-$$"
    info "Creating Debian ${DEBIAN_ARCH} rootfs (debootstrap)..."

    if [ "$NEED_QEMU" -eq 1 ]; then
        # aarch64: two-stage bootstrap
        info "  Stage 1: debootstrap --foreign..."
        debootstrap --arch="$DEBIAN_ARCH" --foreign stable "$BOOTSTRAP_DIR" \
            http://deb.debian.org/debian 2>&1 | sed 's/^/  | /'

        cp "/usr/bin/$QEMU_STATIC" "$BOOTSTRAP_DIR/usr/bin/"
        info "  Stage 2: debootstrap --second-stage..."
        chroot "$BOOTSTRAP_DIR" "/usr/bin/$QEMU_STATIC" \
            /debootstrap/debootstrap --second-stage 2>&1 | sed 's/^/  | /'
        rm -f "$BOOTSTRAP_DIR/usr/bin/$QEMU_STATIC"
    else
        # x86_64: single-stage native
        debootstrap --arch="$DEBIAN_ARCH" stable "$BOOTSTRAP_DIR" \
            http://deb.debian.org/debian 2>&1 | sed 's/^/  | /'
    fi

    # Calculate image size and create
    dir_mb=$(($(du -sm "$BOOTSTRAP_DIR" | cut -f1) + 512))
    img_mb=$((dir_mb + 4096))  # +4GB for source + deps

    info "Creating ext4 image (${img_mb} MiB)..."
    dd if=/dev/zero of="$OUTPUT_IMG" bs=1M count="$img_mb" 2>/dev/null
    mkfs.ext4 -q -O ^metadata_csum,^metadata_csum_seed "$OUTPUT_IMG" 2>/dev/null

    info "Copying rootfs into image..."
    LOOP="$(mount_image "$OUTPUT_IMG" "/mnt/selfhost-rootfs")"
    cp -a "$BOOTSTRAP_DIR"/. "/mnt/selfhost-rootfs/"/
    umount_image "/mnt/selfhost-rootfs" "$LOOP"
    rm -rf "$BOOTSTRAP_DIR"
    info "Base Debian ${DEBIAN_ARCH} image created: $OUTPUT_IMG"

else
    # ── riscv64: use pre-existing base image ────────────────────────────────
    [ -f "$BASE_IMG" ] || die "Base image not found: $BASE_IMG"
    info "Copying base image: $BASE_IMG"
    cp --sparse=always "$BASE_IMG" "$OUTPUT_IMG"
    expand_image "$OUTPUT_IMG" 2048
fi

# ═══════════════════════════════════════════════════════════════════════════════
# Step 2: Install rustc + cargo + build-essential
# ═══════════════════════════════════════════════════════════════════════════════

# Always install toolchain (apt skips already-installed packages)
info "Installing rustc, cargo, build-essential..."
nspawn_run "$OUTPUT_IMG" "
    export DEBIAN_FRONTEND=noninteractive && \
    apt-get update -qq && \
    apt-get install -y -qq --no-install-recommends \
        rustc cargo libstd-rust-dev build-essential ca-certificates curl \
        libclang-dev clang pkgconf libudev-dev && \
    apt-get clean
"
info "Toolchain ready."

# x86_64 self-compile drives the ArceOS std/PIE flow (cargo xtask starry build),
# whose effective target is x86_64-unknown-linux-musl.  Provide a musl C toolchain
# via Debian's standard musl-tools package (lightweight; no custom cross toolchain)
# and the cc/ar/gcc names the std build + lwprintf-rs build.rs expect.
if [ "$ARCH" = "x86_64" ]; then
    info "Installing musl-tools for the x86_64 std build target..."
    nspawn_run "$OUTPUT_IMG" "
        export DEBIAN_FRONTEND=noninteractive && \
        apt-get install -y -qq --no-install-recommends musl-tools musl-dev && \
        apt-get clean && \
        ln -sf /usr/bin/musl-gcc /usr/local/bin/x86_64-linux-musl-cc && \
        ln -sf /usr/bin/musl-gcc /usr/local/bin/x86_64-linux-musl-gcc && \
        ln -sf /usr/bin/ar      /usr/local/bin/x86_64-linux-musl-ar
    "
    info "musl toolchain ready (musl-tools + x86_64-linux-musl-{cc,gcc,ar} symlinks)."
fi

# ─── Install nightly rustc via rustup ─────────────────────────────────────────

RUSTUP_HOST="riscv64gc-unknown-linux-gnu"
if [ "$NEED_QEMU" -eq 0 ]; then
    RUSTUP_HOST="x86_64-unknown-linux-gnu"
elif [ "$ARCH" = "arm" ]; then
    RUSTUP_HOST="aarch64-unknown-linux-gnu"
fi
# Derive the toolchain version from rust-toolchain.toml so it stays in sync
# with the host build.  Fall back to a known-good nightly if the file cannot
# be parsed.
RUSTUP_TOOLCHAIN="$(grep -oP 'channel\s*=\s*"\K[^"]+' "$REPO_ROOT/rust-toolchain.toml" 2>/dev/null || echo 'nightly-2026-05-28')"

info "Installing rustup + $RUSTUP_TOOLCHAIN (host: $RUSTUP_HOST)..."
nspawn_run "$OUTPUT_IMG" "
    apt-get remove -y -qq rustc cargo libstd-rust-dev 2>/dev/null || true && \
    update-ca-certificates 2>/dev/null || true && \
    export RUSTUP_HOME=/root/.rustup CARGO_HOME=/root/.cargo && \
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- \\
        --default-toolchain $RUSTUP_TOOLCHAIN \\
        --profile minimal \\
        --target $RUSTUP_HOST \\
        -y
"
info "Nightly rustc installed (via host rustup)."

# x86_64 self-compile drives the xtask flow, whose kallsyms post-step needs
# gen_ksym + cargo-binutils (rust-nm/rust-objcopy) + llvm-tools.  Pre-install them
# guest-native during the bake (network available) so the offline guest never
# attempts the network auto-install (the inner script also disables that).
if [ "$ARCH" = "x86_64" ]; then
    info "Installing kallsyms tools (llvm-tools-preview, cargo-binutils, ksym)..."
    nspawn_run "$OUTPUT_IMG" "export RUSTUP_HOME=/root/.rustup CARGO_HOME=/root/.cargo PATH=/root/.cargo/bin:\$PATH CARGO_NET_OFFLINE=false && rustup component add llvm-tools-preview && cargo install --locked cargo-binutils && cargo install --locked ksym"
    info "kallsyms tools installed."
fi

# ═══════════════════════════════════════════════════════════════════════════════
# Step 3: Expand image for source tree + cargo registry (~4-6 GB)
# ═══════════════════════════════════════════════════════════════════════════════

CURRENT_MB=$(($(stat --format=%s "$OUTPUT_IMG") / 1024 / 1024))
if [ "$CURRENT_MB" -lt 7000 ]; then
    expand_image "$OUTPUT_IMG" 6144
fi

# ═══════════════════════════════════════════════════════════════════════════════
# Step 4: Copy StarryOS source tree into rootfs
# ═══════════════════════════════════════════════════════════════════════════════

info "Extracting source tree (git archive)..."
TEMP_SRC="$(mktemp -d /tmp/starryos-src.XXXXXX)"
STABLE=""
cleanup_temp() {
    rm -rf "$TEMP_SRC" 2>/dev/null || true
    [ -n "${STABLE:-}" ] && rm -rf "$STABLE" 2>/dev/null || true
}
trap cleanup_temp EXIT

git archive HEAD | tar -x -C "$TEMP_SRC/"
[ -f "$TEMP_SRC/Cargo.toml" ] || die "Cargo.toml missing from git archive"

# git archive has no .git directory, so git rev-parse won't work in the
# guest.  Embed the current HEAD SHA so the guest can verify source identity
# without requiring a git repository.
git rev-parse HEAD > "$TEMP_SRC/.source-commit" 2>/dev/null || \
    die "failed to resolve HEAD — ensure you are in the repo root"
[ -f "$TEMP_SRC/os/StarryOS/kernel/Cargo.toml" ] || die "Kernel Cargo.toml missing"

# The AIC8800 firmware blobs are gitignored (absent from git archive), but xtask
# SHA-256-hashes them before every Starry build and fetches them online if missing.
# Stage the host's blobs into the source tree so the offline guest has them.
if compgen -G "$REPO_ROOT/components/aic8800/firmware/*.bin" >/dev/null 2>&1; then
    mkdir -p "$TEMP_SRC/components/aic8800/firmware"
    cp "$REPO_ROOT"/components/aic8800/firmware/*.bin "$TEMP_SRC/components/aic8800/firmware/"
    info "Staged $(ls "$TEMP_SRC"/components/aic8800/firmware/*.bin | wc -l) AIC8800 firmware blob(s) for offline build."
else
    warn "AIC8800 firmware blobs not found on host; x86_64 self-compile may attempt an online fetch."
fi
chmod -R a+rX "$TEMP_SRC"

info "Copying source tree into rootfs at $DEST_PATH..."

# Bind-mount the source into the container so it's visible inside
STABLE="/tmp/starryos-src-stable"
rm -rf "$STABLE" 2>/dev/null || true
cp -r "$TEMP_SRC" "$STABLE"

nspawn_args=(--image="$OUTPUT_IMG" --bind="$STABLE:$STABLE" --quiet)
if [ "$NEED_QEMU" -eq 1 ] && [ -f "/usr/bin/$QEMU_STATIC" ]; then
    nspawn_args+=(--bind="/usr/bin/$QEMU_STATIC")
fi
[ -n "$NSPAWN_MACHINE" ] && nspawn_args+=(--machine="$NSPAWN_MACHINE")

systemd-nspawn "${nspawn_args[@]}" /usr/bin/bash -c "
    mkdir -p $DEST_PATH && \
    cp -a $STABLE/. $DEST_PATH/ && \
    cp $STABLE/.source-commit $DEST_PATH/.source-commit && \
    chown -R root:root $DEST_PATH && \
    chmod -R a+rX $DEST_PATH
"

rm -rf "$STABLE" 2>/dev/null || true
info "Source tree installed at $DEST_PATH."

# ═══════════════════════════════════════════════════════════════════════════════
# Step 5: Configure .cargo/config.toml for offline target build
# ═══════════════════════════════════════════════════════════════════════════════

info "Configuring /root/.cargo/config.toml for offline ${TARGET} builds..."

nspawn_run "$OUTPUT_IMG" "
    mkdir -p /root/.cargo && \
    cat > /root/.cargo/config.toml << CONFIG_EOF
[net]
offline = true

[build]
target = \"${TARGET}\"
jobs = 1
rustflags = [\"-C\", \"link-arg=--no-rosegment\"]

[target.\"${TARGET}\"]
linker = \"rust-lld\"
CONFIG_EOF
"

# ═══════════════════════════════════════════════════════════════════════════════
# Step 6: Pre-fetch cargo dependencies
# ═══════════════════════════════════════════════════════════════════════════════

# Pre-fetch cargo deps for the offline guest build.
#
# Fetch the FULL closure of the committed Cargo.lock with `--locked`, against
# the unfiltered workspace.  The cache must be a superset of whatever the guest
# resolves: the full-closure fetch is a superset of whatever the guest
# resolves.  The riscv64 guest builds `cargo build -p starryos` (a subset);
# x86_64 builds via xtask against the full workspace — either way the
# full --locked fetch covers all needed crates.
#
# Do NOT regenerate the lockfile from a filtered manifest: that re-resolves to
# different versions than the baked lock (e.g. getrandom 0.3.x instead of the
# locked 0.4.2 -> wasip3 -> wit-bindgen subtree), leaving the offline build
# unable to resolve crates the baked lock references.  `--locked` forbids any
# lock change, so fetch and build see the same graph; the baked Cargo.toml /
# Cargo.lock stay byte-identical to HEAD (source identity is preserved).
info "Pre-fetching cargo dependencies for ${TARGET} (~10-30 min)..."

nspawn_run "$OUTPUT_IMG" "
    export PATH=/root/.cargo/bin:\$PATH && \
    cd $DEST_PATH && \
    cp /root/.cargo/config.toml /root/.cargo/config.toml.bak && \
    printf '[net]\noffline = false\n' > /root/.cargo/config.toml && \
        cargo fetch --locked 2>&1 && \
        cargo fetch --locked --target ${TARGET} 2>&1; \
    RET=\$?; \
    mv /root/.cargo/config.toml.bak /root/.cargo/config.toml; \
    exit \$RET
"
info "Cargo dependencies pre-fetched (full --locked closure; QEMU filters at build time)."

# ═══════════════════════════════════════════════════════════════════════════════
# Step 7: Pre-extract all .crate files to registry/src
# ═══════════════════════════════════════════════════════════════════════════════
# On StarryOS rsext4, reading the registry index (thousands of small files)
# and extracting .crate tarballs is extremely slow due to single-block I/O.
# Pre-extract everything during preparation (Linux ext4) so cargo inside QEMU
# can read pre-extracted source directories directly.
info "Pre-extracting all .crate files to registry/src (~5 min)..."
nspawn_run "$OUTPUT_IMG" "
    export PATH=/root/.cargo/bin:\$PATH && \
    CACHE_DIR=/root/.cargo/registry/cache/index.crates.io-1949cf8c6b5b557f && \
    SRC_DIR=/root/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f && \
    mkdir -p \"\$SRC_DIR\" && \
    cd \"\$CACHE_DIR\" && \
    CRATE_COUNT=0 && \
    CRATE_FAILS=0 && \
    for crate in *.crate; do \
        [ -f \"\$crate\" ] || continue; \
        dirname=\$(basename \"\$crate\" .crate); \
        if [ ! -d \"\$SRC_DIR/\$dirname\" ]; then \
            if tar xf \"\$crate\" -C \"\$SRC_DIR\" 2>/dev/null; then \
                CRATE_COUNT=\$((CRATE_COUNT + 1)); \
            else \
                echo \"WARNING: failed to extract \$crate\" >&2; \
                CRATE_FAILS=\$((CRATE_FAILS + 1)); \
            fi; \
        fi; \
    done && \
    echo \"Pre-extracted \$CRATE_COUNT crates\" && \
    if [ \"\$CRATE_FAILS\" -gt 0 ]; then \
        echo \"WARNING: \$CRATE_FAILS crate(s) failed to extract\" >&2; \
    fi
"
info "Registry pre-extraction complete."

# ═══════════════════════════════════════════════════════════════════════════════
# Step 8: Verify
# ═══════════════════════════════════════════════════════════════════════════════

info "Verifying rootfs..."
PASS=0

check_file() { local label="$1" path="$2"
    if nspawn_run "$OUTPUT_IMG" "test -f $path" &>/dev/null; then
        info "  [OK] $label"
        PASS=$((PASS + 1))
    else
        warn "  [FAIL] $label ($path)"
    fi
}

check_file "workspace Cargo.toml"    "$DEST_PATH/Cargo.toml"
check_file "kernel Cargo.toml"       "$DEST_PATH/os/StarryOS/kernel/Cargo.toml"
check_file "cargo config"            "/root/.cargo/config.toml"
check_file "rustc"                   "/usr/bin/rustc"
check_file "cargo"                   "/usr/bin/cargo"

# ═══════════════════════════════════════════════════════════════════════════════
# Done
# ═══════════════════════════════════════════════════════════════════════════════

FINAL_MB=$(($(stat --format=%s "$OUTPUT_IMG") / 1024 / 1024))
echo ""
echo "  ┌──────────────────────────────────────────────────────────┐"
echo "  │  Self-host rootfs ready                                 │"
echo "  ├──────────────────────────────────────────────────────────┤"
echo "  │  Architecture : $ARCH"
echo "  │  Target       : $TARGET"
echo "  │  Image        : $OUTPUT_IMG"
echo "  │  Size         : ${FINAL_MB} MiB"
echo "  └──────────────────────────────────────────────────────────┘"
echo ""
echo "  Next:"
echo "    ./scripts/self-compile.sh --arch $ARCH"
echo "    ./scripts/run-selfbuilt-kernel.sh --arch $ARCH"
echo ""

# Fix ownership so self-compile.sh (run as regular user) can write via debugfs
if [ -n "${SUDO_USER:-}" ]; then
    chown "$SUDO_USER:$SUDO_USER" "$OUTPUT_IMG" 2>/dev/null || true
    info "Ownership fixed for $SUDO_USER."
fi

echo "$OUTPUT_IMG"
