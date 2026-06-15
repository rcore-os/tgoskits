#!/usr/bin/env bash
#
# Build axloader, install it as EFI/BOOT/BOOTX64.EFI on a USB EFI partition,
# verify the copied file hash, and unmount the partition.
#
# Default target:
#   package: axloader
#   feature: board-asus-nuc15crh
#   target:  x86_64-unknown-uefi
#   output:  BOOTX64.EFI
#   USB fs label: OSTOOLBOOT
#
# Examples:
#   ./bootloader/axloader/scripts/build-install-efi.sh
#   ./bootloader/axloader/scripts/build-install-efi.sh --device /dev/sdb1
#   ./bootloader/axloader/scripts/build-install-efi.sh --no-clean --keep-mounted

set -euo pipefail

SCRIPT_NAME="$(basename "$0")"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"

PACKAGE="axloader"
FEATURE="board-asus-nuc15crh"
TARGET="x86_64-unknown-uefi"
BIN="axloader"
EFI_OUTPUT="BOOTX64.EFI"
USB_LABEL="OSTOOLBOOT"
DEVICE=""
MOUNT_POINT="/tmp/ostool-efi"
CLEAN=1
KEEP_MOUNTED=0
CARGO_BIN="${CARGO:-}"
MOUNTED_BY_SCRIPT=0

info() { printf "[%s] %s\n" "$SCRIPT_NAME" "$*"; }
die() { printf "[%s] ERROR: %s\n" "$SCRIPT_NAME" "$*" >&2; exit 1; }

usage() {
    cat <<EOF
Usage: $SCRIPT_NAME [OPTIONS]

Options:
  --device PATH       EFI partition to mount, for example /dev/sdb1.
  --label LABEL       Find EFI partition by filesystem label. Default: $USB_LABEL.
  --mount-point DIR   Temporary mount point. Default: $MOUNT_POINT.
  --feature FEATURE   axloader board feature. Default: $FEATURE.
  --target TARGET     Rust target. Default: $TARGET.
  --output FILE       EFI output filename under EFI/BOOT. Default: $EFI_OUTPUT.
  --cargo PATH        Cargo executable. Default: \$CARGO, cargo, or /root/.cargo/bin/cargo.
  --no-clean          Skip cargo clean before building.
  --keep-mounted      Do not unmount after writing.
  -h, --help          Show this help.
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --device)
            DEVICE="${2:-}"
            [[ -n "$DEVICE" ]] || die "--device requires a path"
            shift 2
            ;;
        --label)
            USB_LABEL="${2:-}"
            [[ -n "$USB_LABEL" ]] || die "--label requires a value"
            shift 2
            ;;
        --mount-point)
            MOUNT_POINT="${2:-}"
            [[ -n "$MOUNT_POINT" ]] || die "--mount-point requires a directory"
            shift 2
            ;;
        --feature)
            FEATURE="${2:-}"
            [[ -n "$FEATURE" ]] || die "--feature requires a value"
            shift 2
            ;;
        --target)
            TARGET="${2:-}"
            [[ -n "$TARGET" ]] || die "--target requires a value"
            shift 2
            ;;
        --output)
            EFI_OUTPUT="${2:-}"
            [[ -n "$EFI_OUTPUT" ]] || die "--output requires a filename"
            shift 2
            ;;
        --cargo)
            CARGO_BIN="${2:-}"
            [[ -n "$CARGO_BIN" ]] || die "--cargo requires a path"
            shift 2
            ;;
        --no-clean)
            CLEAN=0
            shift
            ;;
        --keep-mounted)
            KEEP_MOUNTED=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            die "unknown option: $1"
            ;;
    esac
done

if [[ -z "$CARGO_BIN" ]]; then
    if command -v cargo >/dev/null 2>&1; then
        CARGO_BIN="cargo"
    elif [[ -x /root/.cargo/bin/cargo ]]; then
        CARGO_BIN="/root/.cargo/bin/cargo"
    else
        die "cargo not found; pass --cargo PATH"
    fi
fi

if [[ -z "$DEVICE" ]]; then
    mapfile -t matches < <(blkid -L "$USB_LABEL" 2>/dev/null || true)
    if [[ "${#matches[@]}" -eq 0 ]]; then
        die "no partition found with label '$USB_LABEL'; pass --device /dev/..."
    fi
    if [[ "${#matches[@]}" -gt 1 ]]; then
        printf "%s\n" "${matches[@]}" >&2
        die "multiple partitions found with label '$USB_LABEL'; pass --device explicitly"
    fi
    DEVICE="${matches[0]}"
fi

[[ -b "$DEVICE" ]] || die "device is not a block device: $DEVICE"

if [[ "$(id -u)" -eq 0 ]]; then
    SUDO=()
else
    command -v sudo >/dev/null 2>&1 || die "sudo is required to mount and write $DEVICE"
    SUDO=(sudo)
fi

cleanup() {
    if [[ "$MOUNTED_BY_SCRIPT" -eq 1 && "$KEEP_MOUNTED" -eq 0 ]]; then
        info "Unmounting $MOUNT_POINT"
        "${SUDO[@]}" umount "$MOUNT_POINT"
    fi
}
trap cleanup EXIT

cd "$REPO_ROOT"

if [[ "$CLEAN" -eq 1 ]]; then
    info "Cleaning $PACKAGE for $TARGET"
    "$CARGO_BIN" clean -p "$PACKAGE" --target "$TARGET"
fi

info "Building $PACKAGE for $TARGET with feature $FEATURE"
"$CARGO_BIN" build \
    -p "$PACKAGE" \
    --target "$TARGET" \
    --features "$FEATURE" \
    --bin "$BIN" \
    --release

LOADER="$REPO_ROOT/target/$TARGET/release/$BIN.efi"
[[ -f "$LOADER" ]] || die "built loader not found: $LOADER"

info "Mounting $DEVICE at $MOUNT_POINT"
"${SUDO[@]}" mkdir -p "$MOUNT_POINT"
if findmnt "$MOUNT_POINT" >/dev/null 2>&1; then
    mounted_source="$(findmnt -n -o SOURCE "$MOUNT_POINT")"
    [[ "$mounted_source" == "$DEVICE" ]] || die "$MOUNT_POINT is already mounted from $mounted_source"
else
    "${SUDO[@]}" mount "$DEVICE" "$MOUNT_POINT"
    MOUNTED_BY_SCRIPT=1
fi

EFI_DIR="$MOUNT_POINT/EFI/BOOT"
TARGET_LOADER="$EFI_DIR/$EFI_OUTPUT"

info "Installing $LOADER to $TARGET_LOADER"
"${SUDO[@]}" mkdir -p "$EFI_DIR"
"${SUDO[@]}" cp "$LOADER" "$TARGET_LOADER"

info "Syncing USB writes"
"${SUDO[@]}" sync

info "Verifying SHA-256"
source_hash="$(sha256sum "$LOADER" | awk '{print $1}')"
target_hash="$(sha256sum "$TARGET_LOADER" | awk '{print $1}')"
printf "source: %s  %s\n" "$source_hash" "$LOADER"
printf "target: %s  %s\n" "$target_hash" "$TARGET_LOADER"
[[ "$source_hash" == "$target_hash" ]] || die "hash mismatch after copy"

info "Installed $EFI_OUTPUT successfully"
