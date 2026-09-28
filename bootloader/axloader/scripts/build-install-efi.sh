#!/usr/bin/env bash
#
# One-time migration of an existing x86_64 ESP to the A/B axloader layout.
#
# Default target:
#   package: axloader
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
STAGING=""

info() { printf "[%s] %s\n" "$SCRIPT_NAME" "$*"; }
die() { printf "[%s] ERROR: %s\n" "$SCRIPT_NAME" "$*" >&2; exit 1; }

usage() {
    cat <<EOF
Usage: $SCRIPT_NAME [OPTIONS]

Options:
  --device PATH       EFI partition to mount, for example /dev/sdb1.
  --label LABEL       Find EFI partition by filesystem label. Default: $USB_LABEL.
  --mount-point DIR   Temporary mount point. Default: $MOUNT_POINT.
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
    if [[ -n "$STAGING" ]]; then rm -rf -- "$STAGING"; fi
    if [[ "$MOUNTED_BY_SCRIPT" -eq 1 && "$KEEP_MOUNTED" -eq 0 ]]; then
        info "Unmounting $MOUNT_POINT"
        "${SUDO[@]}" umount "$MOUNT_POINT"
    fi
}
trap cleanup EXIT

cd "$REPO_ROOT"
[[ "$TARGET" == "x86_64-unknown-uefi" && "$EFI_OUTPUT" == "BOOTX64.EFI" ]] || die "A/B migration currently supports only x86_64-unknown-uefi and BOOTX64.EFI"

if [[ "$CLEAN" -eq 1 ]]; then
    info "Cleaning $PACKAGE for $TARGET"
    "$CARGO_BIN" clean -p "$PACKAGE" --target "$TARGET"
fi

info "Building the loader and immutable launcher for $TARGET"
"$CARGO_BIN" xtask axloader build --target "$TARGET"

LOADER="$REPO_ROOT/target/$TARGET/release/$BIN.efi"
[[ -f "$LOADER" ]] || die "built loader not found: $LOADER"
LAUNCHER="$REPO_ROOT/target/$TARGET/release/axloader-launcher.efi"
[[ -f "$LAUNCHER" ]] || die "built launcher not found: $LAUNCHER"

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
SLOTS="$MOUNT_POINT/EFI/AXLOADER"
[[ "$(findmnt -n -o FSTYPE "$MOUNT_POINT")" == "vfat" ]] || die "EFI partition must be a writable FAT filesystem"
[[ -f "$TARGET_LOADER" ]] || die "existing BOOTX64.EFI required for first migration"
[[ -w "$MOUNT_POINT" ]] || [[ "${#SUDO[@]}" -gt 0 ]] || die "ESP is not writable"
[[ ! -e "$SLOTS/A.EFI" && ! -e "$SLOTS/B.EFI" && ! -e "$SLOTS/STATE0.BIN" && ! -e "$SLOTS/STATE1.BIN" ]] || die "ESP already contains an OTA layout; recover it offline before retrying migration"
[[ ! -e "$SLOTS/BOOTX64.ORIGINAL.EFI" ]] || die "original loader backup exists; recover offline before retrying migration"

STAGING="$(mktemp -d)"
cp -- "$TARGET_LOADER" "$STAGING/A.EFI"
cp -- "$LOADER" "$STAGING/B.EFI"
cp -- "$LAUNCHER" "$STAGING/BOOTX64.EFI"
python3 "$SCRIPT_DIR/init-ota-state.py" --stable "$STAGING/A.EFI" --trial "$STAGING/B.EFI" --output "$STAGING"
for image in "$STAGING/A.EFI" "$STAGING/B.EFI" "$STAGING/BOOTX64.EFI"; do
    python3 - "$image" <<'PY'
import pathlib, sys
b = pathlib.Path(sys.argv[1]).read_bytes()
assert b[:2] == b'MZ' and len(b) >= 128, 'missing DOS header'
p = int.from_bytes(b[0x3c:0x40], 'little')
assert p + 94 <= len(b) and b[p:p+4] == b'PE\0\0', 'missing PE signature'
assert b[p+4:p+6] == b'\x64\x86', 'not an x86_64 EFI image'
assert b[p+24:p+26] == b'\x0b\x02' and b[p+92:p+94] == b'\x0a\x00', 'not an EFI application'
PY
done
required="$(($(stat -c %s "$STAGING/A.EFI") * 2 + $(stat -c %s "$STAGING/B.EFI") + $(stat -c %s "$STAGING/BOOTX64.EFI") + 4194304))"
available="$(df -B1 --output=avail "$MOUNT_POINT" | tail -1 | tr -d ' ')"
(( available > required )) || die "insufficient ESP space: need at least $required bytes available"

info "Preserving the old BOOTX64.EFI and preparing both slots"
"${SUDO[@]}" mkdir -p "$SLOTS"
"${SUDO[@]}" cp -- "$STAGING/A.EFI" "$SLOTS/BOOTX64.ORIGINAL.EFI"
"${SUDO[@]}" cp -- "$STAGING/A.EFI" "$SLOTS/A.EFI"
"${SUDO[@]}" cp -- "$STAGING/B.EFI" "$SLOTS/B.EFI"
"${SUDO[@]}" cp -- "$STAGING/STATE0.BIN" "$SLOTS/STATE0.BIN"
"${SUDO[@]}" cp -- "$STAGING/STATE1.BIN" "$SLOTS/STATE1.BIN"
"${SUDO[@]}" cp -- "$STAGING/BOOTX64.EFI" "$EFI_DIR/BOOTX64.NEW.EFI"
"${SUDO[@]}" sync
for name in A.EFI B.EFI STATE0.BIN STATE1.BIN; do
    [[ "$(sha256sum "$STAGING/$name" | awk '{print $1}')" == "$(sha256sum "$SLOTS/$name" | awk '{print $1}')" ]] || die "staged $name verification failed"
done
[[ "$(sha256sum "$STAGING/A.EFI" | awk '{print $1}')" == "$(sha256sum "$SLOTS/BOOTX64.ORIGINAL.EFI" | awk '{print $1}')" ]] || die "legacy loader backup verification failed"
[[ "$(sha256sum "$STAGING/BOOTX64.EFI" | awk '{print $1}')" == "$(sha256sum "$EFI_DIR/BOOTX64.NEW.EFI" | awk '{print $1}')" ]] || die "launcher staging verification failed"

info "Replacing BOOTX64.EFI (power loss here requires recovery media)"
"${SUDO[@]}" cp -- "$EFI_DIR/BOOTX64.NEW.EFI" "$TARGET_LOADER"

info "Syncing USB writes"
"${SUDO[@]}" sync

info "Verifying SHA-256"
source_hash="$(sha256sum "$STAGING/BOOTX64.EFI" | awk '{print $1}')"
target_hash="$(sha256sum "$TARGET_LOADER" | awk '{print $1}')"
printf "source: %s  %s\n" "$source_hash" "$LAUNCHER"
printf "target: %s  %s\n" "$target_hash" "$TARGET_LOADER"
[[ "$source_hash" == "$target_hash" ]] || die "hash mismatch after copy"

info "Installed launcher with B as a direct-confirmation trial; first update ID printed above"
