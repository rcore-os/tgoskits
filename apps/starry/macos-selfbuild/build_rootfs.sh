#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../../.." && pwd)"

usage() {
    cat <<'USAGE'
Usage:
  apps/starry/macos-selfbuild/build_rootfs.sh \
    [--base-rootfs tmp/axbuild/rootfs/rootfs-aarch64-alpine.img] \
    [--toolchain-rootfs tmp/axbuild/rootfs/rootfs-aarch64-hvf-toolchain.img] \
    [--selfbuild-rootfs tmp/axbuild/rootfs/rootfs-aarch64-hvf-selfbuild.img] \
    [--source .] \
    [--rust-nightly-dir /path/to/aarch64-rust-nightly-sysroot]

Builds the rootfs set used by the macOS HVF self-build app:

  1. ensure/copy the managed AArch64 Alpine rootfs;
  2. resize the copy so it can hold Rust/Cargo and offline Cargo cache;
  3. build an AArch64 Alpine Rust/Cargo payload in Docker;
  4. inject that payload with debugfs to create the toolchain rootfs;
  5. inject the current TGOSKits source tree to create the self-build rootfs.

Docker must be able to run linux/arm64 containers. On Apple Silicon Docker
Desktop this is normally native. On Linux hosts, qemu-user/binfmt may be needed.

Environment:
  DOCKER_IMAGE     Alpine image used for the payload (default: alpine:edge)
  ROOTFS_SIZE_MB   Size of the toolchain image after resize (default: 16384)
  DEBUGFS          Path to debugfs
  E2FSCK           Path to e2fsck
  RESIZE2FS        Path to resize2fs
USAGE
}

base_rootfs="$repo_root/tmp/axbuild/rootfs/rootfs-aarch64-alpine.img"
toolchain_rootfs="$repo_root/tmp/axbuild/rootfs/rootfs-aarch64-hvf-toolchain.img"
selfbuild_rootfs="$repo_root/tmp/axbuild/rootfs/rootfs-aarch64-hvf-selfbuild.img"
source_dir="$repo_root"
rust_nightly_dir="${RUST_NIGHTLY_DIR:-}"
docker_image="${DOCKER_IMAGE:-alpine:edge}"
rootfs_size_mb="${ROOTFS_SIZE_MB:-16384}"

while [[ "$#" -gt 0 ]]; do
    case "$1" in
        --base-rootfs)
            base_rootfs="$2"
            shift 2
            ;;
        --toolchain-rootfs)
            toolchain_rootfs="$2"
            shift 2
            ;;
        --selfbuild-rootfs)
            selfbuild_rootfs="$2"
            shift 2
            ;;
        --source)
            source_dir="$2"
            shift 2
            ;;
        --rust-nightly-dir)
            rust_nightly_dir="$2"
            shift 2
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

find_tool() {
    local env_name="$1"
    local tool_name="$2"
    local homebrew_path="$3"
    local configured="${!env_name:-}"

    if [[ -n "$configured" ]]; then
        printf '%s\n' "$configured"
    elif command -v "$tool_name" >/dev/null 2>&1; then
        command -v "$tool_name"
    elif [[ -x "$homebrew_path" ]]; then
        printf '%s\n' "$homebrew_path"
    else
        echo "$tool_name not found; install e2fsprogs or set $env_name=/path/to/$tool_name" >&2
        exit 1
    fi
}

debugfs="$(find_tool DEBUGFS debugfs /opt/homebrew/opt/e2fsprogs/sbin/debugfs)"
e2fsck="$(find_tool E2FSCK e2fsck /opt/homebrew/opt/e2fsprogs/sbin/e2fsck)"
resize2fs="$(find_tool RESIZE2FS resize2fs /opt/homebrew/opt/e2fsprogs/sbin/resize2fs)"
e2fsprogs_bin="$(dirname "$debugfs")"
export PATH="$e2fsprogs_bin:$PATH"

for cmd in docker cargo tar find; do
    command -v "$cmd" >/dev/null 2>&1 || {
        echo "$cmd not found" >&2
        exit 1
    }
done

if [[ ! -f "$source_dir/Cargo.toml" ]]; then
    echo "source dir does not look like TGOSKits: $source_dir" >&2
    exit 1
fi
source_dir="$(cd "$source_dir" && pwd)"

if [[ ! -f "$base_rootfs" ]]; then
    echo "base rootfs not found: $base_rootfs"
    echo "running: cargo xtask starry rootfs --arch aarch64"
    (cd "$repo_root" && cargo xtask starry rootfs --arch aarch64)
fi

if [[ ! -f "$base_rootfs" ]]; then
    echo "base rootfs still missing after xtask: $base_rootfs" >&2
    exit 1
fi

if [[ -n "$rust_nightly_dir" && ! -d "$rust_nightly_dir" ]]; then
    echo "rust nightly sysroot dir not found: $rust_nightly_dir" >&2
    exit 1
fi

work_dir="$repo_root/target/starry-macos-selfbuild/rootfs-build"
overlay_dir="$work_dir/overlay"
debugfs_cmd="$work_dir/debugfs-build-rootfs.cmd"
debugfs_log="$work_dir/debugfs-build-rootfs.log"
mkdir -p "$work_dir" "$(dirname "$toolchain_rootfs")" "$(dirname "$selfbuild_rootfs")"
rm -rf "$overlay_dir"
mkdir -p "$overlay_dir"

echo "base_rootfs=$base_rootfs"
echo "toolchain_rootfs=$toolchain_rootfs"
echo "selfbuild_rootfs=$selfbuild_rootfs"
echo "source_dir=$source_dir"
echo "docker_image=$docker_image"

echo "copying and resizing base rootfs..."
cp "$base_rootfs" "$toolchain_rootfs"
truncate -s "${rootfs_size_mb}M" "$toolchain_rootfs"
"$e2fsck" -fy "$toolchain_rootfs" >/dev/null 2>&1 || true
"$resize2fs" "$toolchain_rootfs" >/dev/null

echo "building AArch64 Alpine Rust/Cargo payload in Docker..."
docker run --rm --platform linux/arm64 \
    -v "$overlay_dir:/payload" \
    -v "$source_dir:/source:ro" \
    "$docker_image" /bin/sh -euxc '
        apk add --no-cache \
            bash coreutils findutils grep sed gawk tar xz git make cmake pkgconf \
            build-base clang lld llvm rust cargo rust-src

        mkdir -p /payload/usr /payload/lib /payload/root/.cargo /payload/opt
        cp -a /usr/bin /payload/usr/
        cp -a /usr/lib /payload/usr/
        if [ -d /usr/libexec ]; then cp -a /usr/libexec /payload/usr/; fi
        cp -a /lib/. /payload/lib/

        cat >/payload/opt/rustc-nightly-sysroot <<'"'"'WRAP'"'"'
#!/bin/sh
exec /usr/bin/rustc --sysroot /usr "$@"
WRAP
        cat >/payload/opt/rustdoc-nightly-sysroot <<'"'"'WRAP'"'"'
#!/bin/sh
exec /usr/bin/rustdoc --sysroot /usr "$@"
WRAP
        chmod +x /payload/opt/rustc-nightly-sysroot /payload/opt/rustdoc-nightly-sysroot

        cat >/payload/usr/bin/aarch64-linux-musl-gcc <<'"'"'WRAP'"'"'
#!/bin/sh
if [ "${1:-}" = "-print-sysroot" ]; then
    printf '%s\n' /usr
    exit 0
fi

args=""
while [ "$#" -gt 0 ]; do
    case "$1" in
        --target=aarch64-unknown-none|--target=aarch64-unknown-none-softfloat)
            shift
            ;;
        --target)
            if [ "${2:-}" = "aarch64-unknown-none" ] || [ "${2:-}" = "aarch64-unknown-none-softfloat" ]; then
                shift 2
            else
                args="$args $1"
                shift
            fi
            ;;
        *)
            args="$args $1"
            shift
            ;;
    esac
done

exec /usr/bin/gcc $args
WRAP
        chmod +x /payload/usr/bin/aarch64-linux-musl-gcc

        cat >/payload/root/.cargo/config.toml <<'"'"'CARGO_CFG'"'"'
[net]
git-fetch-with-cli = true
CARGO_CFG
        CARGO_HOME=/payload/root/.cargo cargo fetch --manifest-path /source/Cargo.toml
        cat >/payload/root/.cargo/config.toml <<'"'"'CARGO_CFG'"'"'
[net]
git-fetch-with-cli = true
offline = true
CARGO_CFG

        /usr/bin/rustc --version > /payload/opt/toolchain-info.txt
        /usr/bin/cargo --version >> /payload/opt/toolchain-info.txt
    '

if [[ -n "$rust_nightly_dir" ]]; then
    echo "overlaying explicit Rust nightly sysroot: $rust_nightly_dir"
    rm -rf "$overlay_dir/opt/rust-nightly"
    mkdir -p "$overlay_dir/opt/rust-nightly"
    (cd "$rust_nightly_dir" && tar cf - .) | (cd "$overlay_dir/opt/rust-nightly" && tar xf -)
    cat >"$overlay_dir/opt/rustc-nightly-sysroot" <<'WRAP'
#!/bin/sh
exec /opt/rust-nightly/bin/rustc --sysroot /opt/rust-nightly "$@"
WRAP
    cat >"$overlay_dir/opt/rustdoc-nightly-sysroot" <<'WRAP'
#!/bin/sh
exec /opt/rust-nightly/bin/rustdoc --sysroot /opt/rust-nightly "$@"
WRAP
    chmod +x "$overlay_dir/opt/rustc-nightly-sysroot" "$overlay_dir/opt/rustdoc-nightly-sysroot"
fi

echo "injecting toolchain payload with debugfs..."
: >"$debugfs_cmd"
while IFS= read -r path; do
    rel="${path#$overlay_dir/}"
    [[ "$rel" = "$path" ]] && continue
    printf 'mkdir /%s\n' "$rel" >>"$debugfs_cmd"
done < <(find "$overlay_dir" -type d | sort)

while IFS= read -r path; do
    rel="${path#$overlay_dir/}"
    guest_path="/$rel"
    printf 'rm %s\n' "$guest_path" >>"$debugfs_cmd"
    printf 'write %s %s\n' "$path" "$guest_path" >>"$debugfs_cmd"
done < <(find "$overlay_dir" -type f | sort)

while IFS= read -r path; do
    rel="${path#$overlay_dir/}"
    guest_path="/$rel"
    target="$(readlink "$path")"
    printf 'rm %s\n' "$guest_path" >>"$debugfs_cmd"
    printf 'symlink %s %s\n' "$guest_path" "$target" >>"$debugfs_cmd"
done < <(find "$overlay_dir" -type l | sort)

if ! "$debugfs" -w -f "$debugfs_cmd" "$toolchain_rootfs" >"$debugfs_log" 2>&1; then
    cat "$debugfs_log" >&2
    exit 1
fi

echo "injecting TGOSKits source into self-build rootfs..."
"$script_dir/prepare_rootfs.sh" \
    --base-rootfs "$toolchain_rootfs" \
    --output-rootfs "$selfbuild_rootfs" \
    --source "$source_dir"

echo "rootfs build complete"
echo "toolchain_rootfs=$toolchain_rootfs"
echo "selfbuild_rootfs=$selfbuild_rootfs"
