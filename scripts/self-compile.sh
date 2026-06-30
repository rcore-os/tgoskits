#!/usr/bin/env bash
#
# self-compile.sh — Boot StarryOS, compile itself inside QEMU, save the binary.
#
# This is a thin wrapper around `cargo xtask starry app qemu`.  The xtask
# app runner handles: seed kernel build → prebuild overlay generation →
# debugfs overlay injection → QEMU boot.  This script only adds argument
# parsing, env-var forwarding, and post-QEMU binary extraction.
#
# Prerequisites:
#   - Rootfs (run once to create the selfhost rootfs).  Two ways to produce it:
#       (1) --bootstrap (Alpine, no host sudo): provisions the full toolchain
#           (musl, Rust nightly, kallsyms tools, source, firmware) inside QEMU
#           and stops.  Does NOT warm the offline dependency cache, so the
#           followed-up self-compile needs network or the maintainer-prepared
#           rootfs below.
#       (2) Maintainer tool (Debian, requires sudo, warms the offline cache):
#             sudo ./scripts/prepare-selfhost-rootfs.sh --arch x86_64 --force
#     A downloadable pre-built blueprint is planned but not yet available.
#   - qemu-system-<arch>, debugfs (from e2fsprogs)
#
# Usage:
#   ./scripts/self-compile.sh [--arch riscv64|x86_64|aarch64] [--smp N] [--jobs N] \
#                            [--commit SHA] [--ref REF] [--log none|error|warn|info] \
#                            [--bootstrap]
#
#
# Output:
#   Saves the self-compiled starryos binary to tmp/starryos-selfbuilt-<arch>.
#   Run scripts/run-selfbuilt-kernel.sh --arch <arch> to boot it.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

# ─── Argument parsing ───────────────────────────────────────────────────────────

ARCH="riscv64"
SMP=4
CARGO_BUILD_JOBS=""
SELF_COMPILE_COMMIT=""
SELF_COMPILE_REF=""
LOG_LEVEL="info"
BOOTSTRAP="false"

_numeric_level() {
    case "${1:-info}" in
        none|0)  echo 0 ;; error|1) echo 1 ;; warn|2) echo 2 ;; info|3) echo 3 ;;
        *)       echo 3 ;;
    esac
}
_log_allowed() { [ "$(_numeric_level "${LOG_LEVEL}")" -ge "$1" ]; }
info()  { if _log_allowed 3; then printf "[self-compile] %s\n" "$*"; fi; }
warn()  { if _log_allowed 2; then printf "[self-compile] WARN: %s\n" "$*" >&2; fi; }
error() { printf "[self-compile] ERROR: %s\n" "$*" >&2; exit 1; }

while [[ $# -gt 0 ]]; do
    case "$1" in
        --arch)   ARCH="$2"; shift 2 ;;
        --smp)    SMP="$2"; shift 2 ;;
        --jobs)   CARGO_BUILD_JOBS="$2"; shift 2 ;;
        --commit) SELF_COMPILE_COMMIT="$2"; shift 2 ;;
        --ref)    SELF_COMPILE_REF="$2"; shift 2 ;;
        --log)    LOG_LEVEL="${2:-info}"; shift 2 ;;
        --bootstrap)  BOOTSTRAP="true"; shift ;;
        --help|-h)
            echo "Usage: $0 [--arch riscv64|x86_64|aarch64] [--smp N] [--jobs N] [--commit SHA] [--ref REF] [--log none|error|warn|info] [--bootstrap]"
            echo ""
            echo "Options:"
            echo "  --arch <arch>   Target architecture (default: riscv64)"
            echo "  --smp <N>       QEMU CPUs and cargo build jobs (default: 4)"
            echo "  --jobs <N>      Cargo build jobs (default: same as --smp)"
            echo "  --commit <SHA>  Expected source commit for identity verification"
            echo "  --ref <REF>     Expected git ref (informational, no strict check)"
            echo "  --log <level>   Log level: none, error, warn, info (default: info)"
            echo "  --bootstrap     Provision a selfhost rootfs (musl, Rust, kallsyms,
                        source, firmware) from the Alpine base inside QEMU, no
                        host sudo, then stop.  After provisioning, re-run without
                        --bootstrap to self-compile.  The bootstrapped rootfs
                        needs network for the first build (no offline cache)."
            exit 0
            ;;
        *) error "Unknown argument: $1";;
    esac
done

: "${CARGO_BUILD_JOBS:=$SMP}"

# ─── Architecture mapping ───────────────────────────────────────────────────────

case "$ARCH" in
    riscv64)
        SELF_COMPILE_TARGET="riscv64gc-unknown-none-elf"
        ROOTFS_IMG="tmp/axbuild/rootfs/rootfs-riscv64-debian-selfhost-v2.img"
        ;;
    x86_64)
        SELF_COMPILE_TARGET="x86_64-unknown-none"
        ROOTFS_IMG="tmp/selfhost/rootfs-x86_64-selfhost-working.img"
        ;;
    aarch64)
        SELF_COMPILE_TARGET="aarch64-unknown-none-softfloat"
        ROOTFS_IMG="tmp/axbuild/rootfs/rootfs-aarch64-debian-selfhost.img"
        ;;
    *)
        error "Unsupported arch: $ARCH (valid: riscv64, x86_64, aarch64)"
        ;;
esac

# ─── Prerequisite checks + rootfs cloning ──────────────────────────────────────

command -v debugfs &>/dev/null || error "debugfs not found (install e2fsprogs)"

# The xtask app runner modifies the rootfs in-place (injects overlay,
# boots QEMU, guest writes /opt/starryos-selfbuilt).  Clone the
# selfhost rootfs blueprint to a working copy so the blueprint stays
# pristine and the self-compiled binary persists after QEMU exits.
if [ "$ARCH" = "x86_64" ]; then
    mkdir -p tmp/selfhost
    # The selfhost rootfs blueprint is created once by
    # prepare-selfhost-rootfs.sh and reused across runs.  Each run clones
    # it to a working copy so the blueprint stays pristine.
    #
    # --bootstrap PROVISIONS a selfhost rootfs from the Alpine base entirely
    # inside QEMU (no host sudo): build toolchain, Rust, kallsyms tools, full
    # source, AIC8800 firmware, musl symlinks.  It does NOT warm the offline
    # dependency cache (-Zbuild-std), so it is not sufficient for a self-contained
    # offline self-compile on its own.  See apps/starry/selfhost/selfhost-bootstrap/prebuild.sh.
    SELFHOST_BLUEPRINT="tmp/axbuild/rootfs/rootfs-x86_64-selfhost.img"

    if [ ! -f "$SELFHOST_BLUEPRINT" ] && [ "$BOOTSTRAP" = "true" ]; then
        info "=== Bootstrapping selfhost rootfs via QEMU (no host sudo) ==="
        info "This creates an Alpine-based selfhost rootfs with build tools + Rust."
        ALPINE_ROOTFS="tmp/axbuild/rootfs/rootfs-x86_64-alpine.img"
        # The managed-image store nests the image as <dir>/<same-name>; resolve
        # to the actual ext4 file when the path is a directory.
        if [ -d "$ALPINE_ROOTFS" ]; then
            ALPINE_ROOTFS="$ALPINE_ROOTFS/rootfs-x86_64-alpine.img"
        fi
        [ -f "$ALPINE_ROOTFS" ] || error \
            "Alpine rootfs not found: $ALPINE_ROOTFS
Run: cargo xtask starry rootfs --arch x86_64"

        # The bootstrap app's qemu config (selfhost-bootstrap/qemu-x86_64.toml)
        # mounts this NON-managed drive path so the app runner resolves it flat.
        # A path under tmp/axbuild/rootfs/ would instead be rewritten to the
        # managed image-store layout (<store>/<extract-dir>/<name>.img) and would
        # not match the file created here.  Provision into that drive, then
        # relocate to the blueprint once the guest powers off.
        BOOTSTRAP_IMG="tmp/selfhost/rootfs-x86_64-selfhost-bootstrap.img"

        info "Alpine base: $ALPINE_ROOTFS ($(stat -c%s "$ALPINE_ROOTFS") bytes)"
        info "Cloning Alpine base → bootstrap image ($BOOTSTRAP_IMG) ..."
        mkdir -p "$(dirname "$BOOTSTRAP_IMG")"
        cp "$ALPINE_ROOTFS" "$BOOTSTRAP_IMG" || error "Failed to clone Alpine rootfs"
        qemu-img resize -f raw "$BOOTSTRAP_IMG" 16G >/dev/null 2>&1 || true
        if [ "$(stat -c%s "$BOOTSTRAP_IMG")" -lt 3000000000 ]; then
            truncate -s 16G "$BOOTSTRAP_IMG"
        fi
        info "Bootstrap image: $BOOTSTRAP_IMG ($(stat -c%s "$BOOTSTRAP_IMG") bytes)"

        # qemu-img/truncate only enlarge the block device; the ext4 filesystem
        # inside is still the small Alpine-base size.  Grow it to fill the image
        # so the guest has room for the toolchain (apk + rustup + build tools).
        # resize2fs operates on the raw image file directly — no sudo / loop mount.
        info "Growing ext4 filesystem to fill the bootstrap image..."
        e2fsck -fy "$BOOTSTRAP_IMG" >/dev/null 2>&1 || true
        resize2fs "$BOOTSTRAP_IMG" || error "Failed to grow ext4 filesystem in $BOOTSTRAP_IMG"

        info "=== Starting QEMU bootstrap (~15-20 min) ==="
        info "The guest will install build tools + Rust, then power off."

        set +e
        cargo xtask starry app qemu -t selfhost/selfhost-bootstrap --arch "$ARCH"
        BOOTSTRAP_EXIT=$?
        set -e

        if [ "$BOOTSTRAP_EXIT" -ne 0 ]; then
            error "Bootstrap failed (exit=$BOOTSTRAP_EXIT). Retry or use: sudo ./scripts/prepare-selfhost-rootfs.sh --arch x86_64 --force"
        fi

        # Relocate the provisioned image to the blueprint path the full-kernel
        # flow (and the working-copy clone below) consume.
        mkdir -p "$(dirname "$SELFHOST_BLUEPRINT")"
        mv "$BOOTSTRAP_IMG" "$SELFHOST_BLUEPRINT" || error "Failed to relocate bootstrap image to blueprint"
        info "Bootstrap complete: $SELFHOST_BLUEPRINT ($(stat -c%s "$SELFHOST_BLUEPRINT") bytes)"
    fi

    # ─── Blueprint provisioning guidance ────────────────────────────────────
    #
    # When the selfhost blueprint is missing, the no-sudo path is --bootstrap
    # (QEMU-based provisioning from the Alpine base, ~15-20 min).  Once the
    # blueprint exists, re-run without --bootstrap to self-compile.
    #
    # A downloadable pre-built blueprint is planned (tgosimages release) but is
    # not yet published; the download URL and SHA-256 are recorded here for when
    # the release becomes available:
    #
    #   SELFHOST_URL="https://github.com/rcore-os/tgosimages/releases/download/selfhost-rootfs/rootfs-x86_64-selfhost.img.xz"
    #   SELFHOST_SHA256="b17c330958c8970c9db4faec7306182101a0bf34f2b850c80781d63321f49eb8"
    #
    # See docs/starryos-self-compilation.md.

    [ -f "$SELFHOST_BLUEPRINT" ] || error "Selfhost rootfs not found: $SELFHOST_BLUEPRINT

No selfhost blueprint found.  To create one without host sudo, run:

    ./scripts/self-compile.sh --arch x86_64 --bootstrap

This provisions the full toolchain inside QEMU (~15-20 min) and stops.
After provisioning, re-run without --bootstrap to self-compile.

Alternatively, if you have sudo: sudo ./scripts/prepare-selfhost-rootfs.sh --arch x86_64 --force
A downloadable pre-built blueprint is planned but not yet available.

See docs/starryos-self-compilation.md for details."

    # --bootstrap is a PROVISION-ONLY step: it produces the selfhost blueprint
    # (no host sudo) and stops here.  The bootstrapped rootfs has the full
    # toolchain but does NOT have a warmed offline dependency cache, so the
    # follow-up self-compile needs network access (or a warmed-cache rootfs
    # from the maintainer tool).  See docs/starryos-self-compilation.md.
    if [ "$BOOTSTRAP" = "true" ]; then
        printf "[self-compile] Selfhost blueprint provisioned (no host sudo): %s\n" "$SELFHOST_BLUEPRINT"
        info "Provisioning complete.  To self-compile, re-run without --bootstrap."
        info "Note: the follow-up self-compile needs network access (the bootstrap"
        info "rootfs does not include a warmed offline dependency cache)."
        exit 0
    fi

    if [ ! -f "$ROOTFS_IMG" ]; then
        info "Cloning rootfs: $SELFHOST_BLUEPRINT → $ROOTFS_IMG (this may take a moment)..."
        cp "$SELFHOST_BLUEPRINT" "$ROOTFS_IMG" || error "Failed to clone rootfs"
        info "Rootfs clone created ($(stat -c%s "$ROOTFS_IMG") bytes)"
    else
        info "Using existing working copy: $ROOTFS_IMG"
    fi
fi

[ -f "$ROOTFS_IMG" ] || error "Rootfs image not found: $ROOTFS_IMG"

info "Architecture: $ARCH | Target: $SELF_COMPILE_TARGET | SMP: $SMP"

# ─── Forward env vars to prebuild.sh (via xtask app runner) ─────────────────────

export SELF_COMPILE_TARGET
export SELF_COMPILE_ARCH="$ARCH"
export SELF_COMPILE_SMP="$SMP"
export SELF_COMPILE_COMMIT
export SELF_COMPILE_REF
export CARGO_BUILD_JOBS
export REPO_ROOT

# ─── Run via xtask app runner ───────────────────────────────────────────────────

# The app runner handles: defconfig → build → prebuild → overlay inject → QEMU.
# UEFI boot for x86_64 dynamic platform is automatically applied by axbuild.
info "Starting self-compilation via xtask app runner..."
info "Command: cargo xtask starry app qemu -t selfhost/selfhost-full-kernel --arch $ARCH"

set +e
cargo xtask starry app qemu -t selfhost/selfhost-full-kernel --arch "$ARCH"
APP_EXIT=$?
set -e

# ─── Extract self-compiled binary ──────────────────────────────────────────────

CACHED_KERNEL="$REPO_ROOT/tmp/starryos-selfbuilt-${ARCH}"

if [ "$APP_EXIT" -eq 0 ]; then
    # The app runner modifies the rootfs in-place.  Extract the guest-built
    # kernel from the rootfs image via debugfs.
    mkdir -p "$(dirname "$CACHED_KERNEL")"
    # Remove any stale binary from a previous run so a failed extraction
    # cannot be reported as a fresh success by the [ -f && -s ] check below.
    rm -f "$CACHED_KERNEL"
    debugfs -R "dump /opt/starryos-selfbuilt $CACHED_KERNEL" "$ROOTFS_IMG" 2>/dev/null || true

    if [ -f "$CACHED_KERNEL" ] && [ -s "$CACHED_KERNEL" ]; then
        BINARY_SIZE=$(stat -c%s "$CACHED_KERNEL")
        printf "[self-compile] SUCCESS — self-compiled kernel: %s (%s bytes)\n" "$CACHED_KERNEL" "$BINARY_SIZE"
        info "Run: ./scripts/run-selfbuilt-kernel.sh --arch $ARCH"
        info "  or: ./scripts/run-selfbuilt-kernel.sh --arch $ARCH --kernel $CACHED_KERNEL"
    else
        error "App runner exited successfully but /opt/starryos-selfbuilt not found in rootfs"
    fi
else
    error "Self-compilation FAILED (xtask exit code: $APP_EXIT)"
fi
