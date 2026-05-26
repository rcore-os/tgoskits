#!/usr/bin/env bash
set -euo pipefail

usage() {
    cat <<'USAGE'
Usage:
  apps/starry/macos-selfbuild/check_rootfs.sh <rootfs.img>

Checks whether a prepared StarryOS self-build rootfs contains the minimum guest
paths needed by run_selfbuild.sh.
USAGE
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
    usage
    exit 0
fi

rootfs="${1:-}"
if [[ -z "$rootfs" ]]; then
    usage >&2
    exit 2
fi

if [[ ! -f "$rootfs" ]]; then
    echo "rootfs not found: $rootfs" >&2
    exit 1
fi

debugfs="${DEBUGFS:-}"
if [[ -z "$debugfs" ]]; then
    if command -v debugfs >/dev/null 2>&1; then
        debugfs="$(command -v debugfs)"
    elif [[ -x /opt/homebrew/opt/e2fsprogs/sbin/debugfs ]]; then
        debugfs="/opt/homebrew/opt/e2fsprogs/sbin/debugfs"
    else
        echo "debugfs not found; install e2fsprogs or set DEBUGFS=/path/to/debugfs" >&2
        exit 1
    fi
fi

missing=0
for guest_path in "/usr/bin/cargo" "/opt/rustc-nightly-sysroot" "/opt/rustdoc-nightly-sysroot"; do
    if "$debugfs" -R "stat $guest_path" "$rootfs" >/dev/null 2>&1; then
        echo "OK $guest_path"
    else
        echo "MISSING $guest_path"
        missing=1
    fi
done

source_ok=0
for guest_path in "/opt/tgoskits/Cargo.toml" "/opt/tgoskits-src.tar"; do
    if "$debugfs" -R "stat $guest_path" "$rootfs" >/dev/null 2>&1; then
        echo "OK $guest_path"
        source_ok=1
    else
        echo "MISSING $guest_path"
    fi
done

if [[ "$source_ok" = "0" ]]; then
    missing=1
fi

if [[ "$missing" = "0" ]]; then
    echo "STARRY_MACOS_SELFBUILD_ROOTFS_OK"
else
    echo "STARRY_MACOS_SELFBUILD_ROOTFS_INCOMPLETE"
fi

exit "$missing"
