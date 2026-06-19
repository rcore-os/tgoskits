#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../../.." && pwd)"

usage() {
    cat <<'USAGE'
Usage:
  apps/starry/macos-selfbuild/reproduce.sh [extra cargo xtask starry app qemu args]

Runs the complete Apple Silicon macOS AArch64 self-build reproduction through xtask:

  1. build the seed StarryOS kernel and host-generated bindings;
  2. pull and resize the managed AArch64 Alpine rootfs through xtask image;
  3. prepare the app-local guest toolchain overlay cache;
  4. launch `cargo xtask starry app qemu`, letting xtask inject the overlay and
     patch the rootfs/QEMU config.

Environment:
  ROOTFS_MODE               build-rootfs|skip (default: build-rootfs)
  TGOS_IMAGE_LOCAL_STORAGE  Image storage directory
  ROOTFS_SIZE_MIB           Final managed image size in MiB (default: 16384)
  ARTIFACT_EXTRACT          1|0, extract guest-built artifacts from rootfs (default: 1)
  ARTIFACT_UPLOAD           1|0, receive guest-built artifacts over network (default: 0)
  ARTIFACT_UPLOAD_PORT      Host upload receiver port (default: 18180)
USAGE
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
    usage
    exit 0
fi

if [[ "$(uname -s)" != "Darwin" || "$(uname -m)" != "arm64" ]]; then
    echo "warning: this workflow is intended for Apple Silicon macOS with QEMU AArch64 TCG" >&2
fi

rootfs_mode="${ROOTFS_MODE:-build-rootfs}"
artifact_extract="${ARTIFACT_EXTRACT:-1}"
artifact_upload="${ARTIFACT_UPLOAD:-0}"
artifact_upload_port="${ARTIFACT_UPLOAD_PORT:-18180}"
artifact_upload_bind="${ARTIFACT_UPLOAD_BIND:-127.0.0.1}"
artifact_upload_guest_host="${ARTIFACT_UPLOAD_GUEST_HOST:-10.0.2.2}"
artifact_upload_dir="${ARTIFACT_UPLOAD_DIR:-$repo_root/target/starry-macos-selfbuild/uploaded}"
artifact_upload_log="${ARTIFACT_UPLOAD_LOG:-$repo_root/target/starry-macos-selfbuild/artifact-upload-server.log}"
artifact_upload_pid=""
export TGOS_IMAGE_LOCAL_STORAGE="${TGOS_IMAGE_LOCAL_STORAGE:-$repo_root/target/starry-macos-selfbuild/tgos-images}"
rootfs_image="${ROOTFS:-$TGOS_IMAGE_LOCAL_STORAGE/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img}"

cleanup() {
    if [[ -n "$artifact_upload_pid" ]]; then
        kill "$artifact_upload_pid" 2>/dev/null || true
        wait "$artifact_upload_pid" 2>/dev/null || true
    fi
}
trap cleanup EXIT

make_upload_token() {
    if command -v openssl >/dev/null 2>&1; then
        openssl rand -hex 16
    elif command -v uuidgen >/dev/null 2>&1; then
        uuidgen | tr '[:upper:]' '[:lower:]' | tr -d '-'
    else
        printf '%s-%s\n' "$$" "$(date +%s)"
    fi
}

start_artifact_upload_server() {
    local token ready_url i

    mkdir -p "$artifact_upload_dir" "$(dirname "$artifact_upload_log")"
    token="${ARTIFACT_UPLOAD_TOKEN:-$(make_upload_token)}"

    "$script_dir/artifact_upload_server.py" \
        --bind "$artifact_upload_bind" \
        --port "$artifact_upload_port" \
        --dir "$artifact_upload_dir" \
        --token "$token" \
        >"$artifact_upload_log" 2>&1 &
    artifact_upload_pid="$!"

    ready_url="http://${artifact_upload_bind}:${artifact_upload_port}/health"
    for ((i = 0; i < 100; i++)); do
        if curl -fsS "$ready_url" >/dev/null 2>&1; then
            export ARTIFACT_UPLOAD_URL="http://${artifact_upload_guest_host}:${artifact_upload_port}/upload"
            export ARTIFACT_UPLOAD_TOKEN="$token"
            export ARTIFACT_UPLOAD_REQUIRED="${ARTIFACT_UPLOAD_REQUIRED:-1}"
            echo "artifact_upload_dir=$artifact_upload_dir"
            echo "artifact_upload_log=$artifact_upload_log"
            return
        fi
        if ! kill -0 "$artifact_upload_pid" 2>/dev/null; then
            cat "$artifact_upload_log" >&2 || true
            echo "artifact upload server exited before becoming ready" >&2
            exit 1
        fi
        sleep 0.1
    done

    cat "$artifact_upload_log" >&2 || true
    echo "artifact upload server did not become ready at $ready_url" >&2
    exit 1
}

check_uploaded_artifacts() {
    local target bin stem elf

    [[ "$artifact_upload" = "1" ]] || return 0
    target="${BUILD_TARGET:-aarch64-unknown-none-softfloat}"
    stem="${BUILD_BIN:-starryos}-${target}"
    elf="$artifact_upload_dir/$stem"
    bin="$artifact_upload_dir/$stem.bin"

    if [[ ! -s "$elf" || ! -s "$bin" ]]; then
        echo "missing uploaded self-build artifacts under $artifact_upload_dir" >&2
        [[ -s "$elf" ]] || echo "  missing: $elf" >&2
        [[ -s "$bin" ]] || echo "  missing: $bin" >&2
        return 1
    fi

    echo "uploaded_kernel_elf=$elf"
    echo "uploaded_kernel_bin=$bin"
}

find_tool() {
    local env_value="$1"
    shift

    if [[ -n "$env_value" && -x "$env_value" ]]; then
        printf '%s\n' "$env_value"
        return 0
    fi
    for candidate in "$@"; do
        if command -v "$candidate" >/dev/null 2>&1; then
            command -v "$candidate"
            return 0
        fi
        if [[ -x "$candidate" ]]; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done
    return 1
}

extract_rootfs_artifacts() {
    local debugfs e2fsck fsck_rc target stem elf_out bin_out

    [[ "$artifact_extract" = "1" ]] || return 0
    if [[ ! -f "$rootfs_image" ]]; then
        echo "rootfs image not found for artifact extraction: $rootfs_image" >&2
        return 1
    fi

    debugfs="$(find_tool "${DEBUGFS:-}" debugfs /opt/homebrew/opt/e2fsprogs/sbin/debugfs)"
    e2fsck="$(find_tool "${E2FSCK:-}" e2fsck /opt/homebrew/opt/e2fsprogs/sbin/e2fsck)"

    set +e
    "$e2fsck" -fy "$rootfs_image"
    fsck_rc="$?"
    set -e
    if (( (fsck_rc & ~3) != 0 )); then
        echo "e2fsck failed for $rootfs_image with rc=$fsck_rc" >&2
        return "$fsck_rc"
    fi

    mkdir -p "$artifact_upload_dir"
    target="${BUILD_TARGET:-aarch64-unknown-none-softfloat}"
    stem="${BUILD_BIN:-starryos}-${target}"
    elf_out="$artifact_upload_dir/$stem"
    bin_out="$artifact_upload_dir/$stem.bin"
    rm -f "$elf_out" "$bin_out"

    "$debugfs" -R "dump -p /opt/starryos-selfbuild-artifacts/$stem $elf_out" "$rootfs_image"
    "$debugfs" -R "dump -p /opt/starryos-selfbuild-artifacts/$stem.bin $bin_out" "$rootfs_image"

    if [[ ! -s "$elf_out" || ! -s "$bin_out" ]]; then
        echo "missing extracted self-build artifacts under $artifact_upload_dir" >&2
        [[ -s "$elf_out" ]] || echo "  missing: $elf_out" >&2
        [[ -s "$bin_out" ]] || echo "  missing: $bin_out" >&2
        return 1
    fi

    echo "extracted_kernel_elf=$elf_out"
    echo "extracted_kernel_bin=$bin_out"
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

if [[ "$artifact_upload" = "1" ]]; then
    start_artifact_upload_server
fi

cd "$repo_root"
set +e
cargo xtask starry app qemu \
    -t macos-selfbuild \
    --arch aarch64 \
    --qemu-config "$script_dir/qemu-aarch64.toml" \
    "$@"
qemu_rc="$?"
set -e

if [[ "$qemu_rc" = "0" ]]; then
    extract_rootfs_artifacts
    check_uploaded_artifacts
fi
exit "$qemu_rc"
