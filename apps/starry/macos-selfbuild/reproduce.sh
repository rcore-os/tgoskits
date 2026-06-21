#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../../.." && pwd)"

usage() {
    cat <<'USAGE'
Usage:
  apps/starry/macos-selfbuild/reproduce.sh

Runs the complete Apple Silicon macOS AArch64 self-build reproduction:

  1. build the seed StarryOS kernel;
  2. pull and resize the managed AArch64 Alpine rootfs through xtask image;
  3. prepare the app-local guest toolchain overlay cache;
  4. copy the rootfs, inject this app's overlay, self-build in QEMU, and extract
     the guest-built kernel from the copied rootfs.

Environment:
  ROOTFS_MODE               build-rootfs|skip (default: build-rootfs)
  TGOS_IMAGE_LOCAL_STORAGE  Image storage directory
  ROOTFS_SIZE_MIB           Final managed image size in MiB (default: 16384)
  ARTIFACT_OUT_DIR          Host output directory for extracted artifacts
  PREPARE_OVERLAY           1|0, inject app overlay into the copied rootfs (default: 1)
USAGE
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
    usage
    exit 0
fi

if [[ "$(uname -s)" != "Darwin" || "$(uname -m)" != "arm64" ]]; then
    echo "warning: this workflow is intended for Apple Silicon macOS with QEMU AArch64 HVF" >&2
fi

rootfs_mode="${ROOTFS_MODE:-build-rootfs}"
artifact_out_dir="${ARTIFACT_OUT_DIR:-$repo_root/target/starry-macos-selfbuild/uploaded}"
export TGOS_IMAGE_LOCAL_STORAGE="${TGOS_IMAGE_LOCAL_STORAGE:-$repo_root/target/starry-macos-selfbuild/tgos-images}"

check_extracted_artifacts() {
    local target bin stem elf

    [[ "${ARTIFACT_EXTRACT:-1}" = "1" ]] || return 0
    target="aarch64-unknown-none-softfloat"
    stem="starryos-${target}"
    elf="$artifact_out_dir/$stem"
    bin="$artifact_out_dir/$stem.bin"

    if [[ ! -s "$elf" || ! -s "$bin" ]]; then
        echo "missing extracted self-build artifacts under $artifact_out_dir" >&2
        [[ -s "$elf" ]] || echo "  missing: $elf" >&2
        [[ -s "$bin" ]] || echo "  missing: $bin" >&2
        return 1
    fi

    echo "extracted_kernel_elf=$elf"
    echo "extracted_kernel_bin=$bin"
}

"$script_dir/build_kernel.sh"

case "$rootfs_mode" in
    build-rootfs)
        "$script_dir/build_rootfs.sh"
        ;;
    skip)
        ;;
    *)
        echo "unknown ROOTFS_MODE=$rootfs_mode; expected build-rootfs or skip" >&2
        exit 2
        ;;
esac

source "$script_dir/prepare_host_tools.sh"
prepare_macos_selfbuild_host_tools

export ARTIFACT_OUT_DIR="$artifact_out_dir"
"$script_dir/run_selfbuild.sh"
qemu_rc="$?"

if [[ "$qemu_rc" = "0" ]]; then
    check_extracted_artifacts
fi
exit "$qemu_rc"
