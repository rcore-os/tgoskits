#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../../.." && pwd)"

usage() {
    cat <<'USAGE'
Usage:
  apps/starry/macos-selfbuild/full_self_build.sh

Runs the complete Apple Silicon macOS AArch64 self-build flow. This is the
user-facing entrypoint for the default end-to-end run.

Stages:
  1. prepare host-side tools needed by the AArch64 seed kernel build;
  2. run `cargo xtask starry app qemu -t macos-selfbuild --arch aarch64`;
  3. extract the guest-built kernel from the rootfs used by the app runner.

Environment:
  ROOTFS_SIZE_MIB   Final app runner rootfs size in MiB (default: 16384)
  ARTIFACT_OUT_DIR  Host output directory for extracted artifacts
  ARTIFACT_EXTRACT  1|0, extract artifacts after app qemu succeeds (default: 1)
USAGE
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
    usage
    exit 0
fi

if [[ "$(uname -s)" != "Darwin" || "$(uname -m)" != "arm64" ]]; then
    echo "warning: this workflow is intended for Apple Silicon macOS with QEMU AArch64 HVF" >&2
fi

artifact_out_dir="${ARTIFACT_OUT_DIR:-$repo_root/target/starry-macos-selfbuild/uploaded}"
rootfs_path_file="$repo_root/target/starry-macos-selfbuild/rootfs.path"

find_tool() {
    local override="$1"
    local name="$2"
    shift 2

    if [[ -n "$override" ]]; then
        if [[ -x "$override" ]]; then
            printf '%s\n' "$override"
            return 0
        fi
        echo "$name override is not executable: $override" >&2
        return 1
    fi

    if command -v "$name" >/dev/null 2>&1; then
        command -v "$name"
        return 0
    fi

    local candidate
    for candidate in "$@"; do
        if [[ -x "$candidate" ]]; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done

    echo "$name not found; install e2fsprogs or set ${name^^}" >&2
    return 1
}

fsck_rootfs() {
    local rootfs="$1"
    local e2fsck rc
    e2fsck="$(find_tool "${E2FSCK:-}" e2fsck \
        /opt/homebrew/opt/e2fsprogs/sbin/e2fsck \
        /usr/local/opt/e2fsprogs/sbin/e2fsck)"

    set +e
    "$e2fsck" -fy "$rootfs"
    rc="$?"
    set -e
    if (( (rc & ~3) != 0 )); then
        echo "e2fsck failed for $rootfs with exit code $rc" >&2
        return "$rc"
    fi
}

dump_guest_artifact() {
    local debugfs="$1"
    local rootfs="$2"
    local guest_path="$3"
    local host_path="$4"

    rm -f "$host_path"
    "$debugfs" -R "dump -p $guest_path $host_path" "$rootfs"
    if [[ ! -s "$host_path" ]]; then
        echo "failed to extract $guest_path to $host_path" >&2
        return 1
    fi
}

extract_guest_artifacts() {
    local rootfs debugfs target stem elf bin guest_dir

    [[ "${ARTIFACT_EXTRACT:-1}" = "1" ]] || return 0
    if [[ ! -f "$rootfs_path_file" ]]; then
        echo "missing app runner rootfs path record: $rootfs_path_file" >&2
        return 1
    fi
    IFS= read -r rootfs <"$rootfs_path_file"
    if [[ ! -f "$rootfs" ]]; then
        echo "recorded app runner rootfs does not exist: $rootfs" >&2
        return 1
    fi

    debugfs="$(find_tool "${DEBUGFS:-}" debugfs \
        /opt/homebrew/opt/e2fsprogs/sbin/debugfs \
        /usr/local/opt/e2fsprogs/sbin/debugfs)"

    target="aarch64-unknown-none-softfloat"
    stem="starryos-${target}"
    elf="$artifact_out_dir/$stem"
    bin="$artifact_out_dir/$stem.bin"
    guest_dir="/opt/starryos-selfbuild-artifacts"

    mkdir -p "$artifact_out_dir"
    fsck_rootfs "$rootfs"
    dump_guest_artifact "$debugfs" "$rootfs" "$guest_dir/$stem" "$elf"
    dump_guest_artifact "$debugfs" "$rootfs" "$guest_dir/$stem.bin" "$bin"
}

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

source "$script_dir/prepare_host_tools.sh"
prepare_macos_selfbuild_host_tools

export ARTIFACT_OUT_DIR="$artifact_out_dir"
if (cd "$repo_root" && cargo xtask starry app qemu -t macos-selfbuild --arch aarch64); then
    extract_guest_artifacts
    check_extracted_artifacts
else
    exit "$?"
fi
