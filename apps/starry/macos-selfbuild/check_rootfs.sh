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
path_exists() {
    local guest_path="$1"
    local out

    out="$("$debugfs" -R "stat $guest_path" "$rootfs" 2>&1 || true)"
    if printf '%s\n' "$out" | grep -E -q 'File not found|Ext2 inode is not a|ext2_lookup|No such file'; then
        return 1
    fi
    printf '%s\n' "$out" | grep -q 'Inode:'
}

dir_contains() {
    local guest_dir="$1"
    local pattern="$2"
    local out

    out="$("$debugfs" -R "ls $guest_dir" "$rootfs" 2>&1 || true)"
    printf '%s\n' "$out" | grep -q "$pattern"
}

for guest_path in \
    "/usr/bin/cargo" \
    "/opt/rust-nightly/bin/rustc" \
    "/opt/rust-nightly/lib/rustlib/src/rust/library/Cargo.lock" \
    "/usr/bin/aarch64-linux-musl-gcc" \
    "/opt/cargo-nightly-sysroot" \
    "/opt/rustc-nightly-sysroot" \
    "/opt/rustdoc-nightly-sysroot"; do
    if path_exists "$guest_path"; then
        echo "OK $guest_path"
    else
        echo "MISSING $guest_path"
        missing=1
    fi
done

if dir_contains "/opt/rust-nightly/lib" "librustc_driver-.*\\.so"; then
    echo "OK /opt/rust-nightly/lib/librustc_driver-*.so"
else
    echo "MISSING /opt/rust-nightly/lib/librustc_driver-*.so"
    missing=1
fi

if path_exists "/usr/lib/libclang.so" || path_exists "/usr/lib/llvm22/lib/libclang.so"; then
    echo "OK libclang.so"
else
    echo "MISSING libclang.so"
    missing=1
fi

source_ok=0
for guest_path in "/opt/tgoskits/Cargo.toml" "/opt/tgoskits-src.tar"; do
    if path_exists "$guest_path"; then
        echo "OK $guest_path"
        source_ok=1
    else
        echo "MISSING $guest_path"
    fi
done

if [[ "$source_ok" = "0" ]]; then
    missing=1
fi

deps_ok=0
if path_exists "/opt/tgoskits/vendor"; then
    echo "OK /opt/tgoskits/vendor"
    deps_ok=1
elif path_exists "/root/.cargo/registry/index" \
    && path_exists "/root/.cargo/registry/cache"; then
    echo "OK /root/.cargo/registry/index"
    echo "OK /root/.cargo/registry/cache"
    deps_ok=1
else
    echo "MISSING offline Cargo dependencies (/opt/tgoskits/vendor or /root/.cargo/registry)"
fi

if [[ "$deps_ok" = "0" ]]; then
    missing=1
fi

if [[ "$missing" = "0" ]]; then
    echo "STARRY_MACOS_SELFBUILD_ROOTFS_OK"
else
    echo "STARRY_MACOS_SELFBUILD_ROOTFS_INCOMPLETE"
fi

exit "$missing"
