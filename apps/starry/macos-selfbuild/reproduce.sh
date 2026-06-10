#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../../.." && pwd)"

usage() {
    cat <<'USAGE'
Usage:
  apps/starry/macos-selfbuild/reproduce.sh

Runs the complete macOS HVF self-build reproduction:
  1. build or refresh the AArch64 self-build rootfs;
  2. build the seed StarryOS kernel;
  3. boot StarryOS with QEMU HVF and build StarryOS inside the guest.

Common knobs:
  SMP=8 JOBS=8 MEM=4096M QEMU_TIMEOUT_SEC=10800
  ROOTFS_MODE=build-rootfs|prepare-rootfs|skip
  RUST_DIST_SERVER=https://rsproxy.cn
  STARRY_CARGO_REGISTRY_INDEX=sparse+https://rsproxy.cn/index/

For memory-constrained base M1 machines, first verify the flow with:
  SMP=4 JOBS=4 MEM=3072M apps/starry/macos-selfbuild/reproduce.sh
USAGE
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
    usage
    exit 0
fi

if [[ "$(uname -s)" != "Darwin" || "$(uname -m)" != "arm64" ]]; then
    echo "warning: this workflow is intended for Apple Silicon macOS with QEMU HVF" >&2
fi

rootfs_mode="${ROOTFS_MODE:-build-rootfs}"
kernel="${KERNEL:-$repo_root/target/aarch64-unknown-none-softfloat/release/starryos.bin}"
rootfs="${ROOTFS:-$repo_root/tmp/axbuild/rootfs/rootfs-aarch64-hvf-selfbuild.img}"

case "$rootfs_mode" in
    build-rootfs)
        "$script_dir/build_rootfs.sh"
        ;;
    prepare-rootfs)
        "$script_dir/prepare_rootfs.sh"
        ;;
    skip)
        ;;
    *)
        echo "unknown ROOTFS_MODE=$rootfs_mode; expected build-rootfs, prepare-rootfs, or skip" >&2
        exit 2
        ;;
esac

"$script_dir/build_kernel.sh"

exec env \
    KERNEL="$kernel" \
    ROOTFS="$rootfs" \
    SMP="${SMP:-8}" \
    JOBS="${JOBS:-${SMP:-8}}" \
    MEM="${MEM:-4096M}" \
    RAYON_NUM_THREADS="${RAYON_NUM_THREADS:-1}" \
    RUSTC_THREADS="${RUSTC_THREADS:-2}" \
    SOURCE_TMPFS="${SOURCE_TMPFS:-1}" \
    TARGET_SPEC_MODE="${TARGET_SPEC_MODE:-pie}" \
    ARTIFACT_TO_BIN="${ARTIFACT_TO_BIN:-1}" \
    TARGET_HEARTBEAT_SEC="${TARGET_HEARTBEAT_SEC:-0}" \
    TRACE_RUSTC="${TRACE_RUSTC:-0}" \
    CARGO_VERBOSE="${CARGO_VERBOSE:-0}" \
    EXPECTED_MAX_CRATES="${EXPECTED_MAX_CRATES:-420}" \
    QEMU_TIMEOUT_SEC="${QEMU_TIMEOUT_SEC:-10800}" \
    "$script_dir/run_selfbuild.sh"
