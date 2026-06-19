#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../../.." && pwd)"

storage_dir="${TGOS_IMAGE_LOCAL_STORAGE:-$repo_root/target/starry-macos-selfbuild/tgos-images}"
rootfs_size_mib="${ROOTFS_SIZE_MIB:-16384}"
arch="${ARCH:-aarch64}"
force_toolchain=0

usage() {
    cat <<'USAGE'
Usage:
  apps/starry/macos-selfbuild/build_rootfs.sh [--size-mib MIB] [--force-toolchain]

Prepares the rootfs inputs for the macOS HVF self-build app without doing any
manual rootfs injection:

  1. pulls the managed AArch64 Alpine rootfs through xtask image storage;
  2. grows that managed image with `cargo xtask image resize`;
  3. prepares the app-local guest toolchain overlay cache under target/.

The actual overlay injection is still done by `cargo xtask starry app qemu`,
which runs this app's prebuild.sh and then calls the existing xtask
inject_overlay path.

Environment:
  TGOS_IMAGE_LOCAL_STORAGE  Image storage directory
  ROOTFS_SIZE_MIB           Final managed image size in MiB (default: 16384)
USAGE
}

while [[ "$#" -gt 0 ]]; do
    case "$1" in
        --size-mib)
            rootfs_size_mib="$2"
            shift 2
            ;;
        --storage)
            storage_dir="$2"
            shift 2
            ;;
        --force-toolchain)
            force_toolchain=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

mkdir -p "$storage_dir"
export TGOS_IMAGE_LOCAL_STORAGE="$storage_dir"

cd "$repo_root"
cargo xtask image pull --arch "$arch"

rootfs="$storage_dir/rootfs-${arch}-alpine.img/rootfs-${arch}-alpine.img"
if [[ ! -f "$rootfs" ]]; then
    echo "managed rootfs was not found after pull: $rootfs" >&2
    exit 1
fi

cargo xtask image resize "$rootfs" --size-mib "$rootfs_size_mib"

toolchain_args=(--output "$repo_root/target/starry-macos-selfbuild/rootfs-build/toolchain-overlay")
if [[ "$force_toolchain" = "1" ]]; then
    toolchain_args+=(--force)
fi
"$script_dir/prepare_toolchain_overlay.sh" "${toolchain_args[@]}"

cat <<EOF
rootfs=$rootfs
toolchain_overlay=$repo_root/target/starry-macos-selfbuild/rootfs-build/toolchain-overlay
overlay_injection=cargo xtask starry app qemu -t macos-selfbuild --arch aarch64
EOF
