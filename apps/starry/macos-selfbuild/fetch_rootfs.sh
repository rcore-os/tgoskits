#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../../.." && pwd)"

usage() {
    cat <<'USAGE'
Usage:
  ROOTFS_URL=https://.../rootfs-aarch64-hvf-selfbuild.img \
    apps/starry/macos-selfbuild/fetch_rootfs.sh

  apps/starry/macos-selfbuild/fetch_rootfs.sh \
    --input /path/to/rootfs-aarch64-hvf-selfbuild.img

  apps/starry/macos-selfbuild/fetch_rootfs.sh \
    --url https://.../rootfs-aarch64-hvf-selfbuild.img.tar.xz

Places a prebuilt macOS/HVF self-build rootfs at:

  tmp/axbuild/rootfs/rootfs-aarch64-hvf-selfbuild.img

This is the normal reproduction path. It does not build the rootfs and does not
use Docker.
USAGE
}

url="${ROOTFS_URL:-}"
input="${ROOTFS_INPUT:-}"
output="$repo_root/tmp/axbuild/rootfs/rootfs-aarch64-hvf-selfbuild.img"

while [[ "$#" -gt 0 ]]; do
    case "$1" in
        --url)
            url="$2"
            shift 2
            ;;
        --input)
            input="$2"
            shift 2
            ;;
        --output)
            output="$2"
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

if [[ -n "$url" && -n "$input" ]]; then
    echo "use either --url/ROOTFS_URL or --input/ROOTFS_INPUT, not both" >&2
    exit 2
fi

if [[ -z "$url" && -z "$input" ]]; then
    usage >&2
    exit 2
fi

copy_image() {
    local src="$1"
    local dst="$2"

    rm -f "$dst"
    if cp -c "$src" "$dst" 2>/dev/null; then
        return
    fi
    if cp --reflink=auto "$src" "$dst" 2>/dev/null; then
        return
    fi
    cp "$src" "$dst"
}

mkdir -p "$(dirname "$output")" "$repo_root/target/starry-macos-selfbuild"

if [[ -n "$input" ]]; then
    if [[ ! -f "$input" ]]; then
        echo "rootfs input not found: $input" >&2
        exit 1
    fi
    copy_image "$input" "$output"
else
    download="$repo_root/target/starry-macos-selfbuild/$(basename "${url%%\?*}")"
    curl -fL --retry 5 --retry-delay 3 --retry-all-errors -o "$download" "$url"
    case "$download" in
        *.tar.xz|*.txz)
            tar -xJf "$download" -C "$(dirname "$output")"
            ;;
        *.tar.gz|*.tgz)
            tar -xzf "$download" -C "$(dirname "$output")"
            ;;
        *.img)
            copy_image "$download" "$output"
            ;;
        *)
            echo "unsupported rootfs artifact extension: $download" >&2
            echo "expected .img, .tar.xz, .txz, .tar.gz, or .tgz" >&2
            exit 1
            ;;
    esac
fi

if [[ ! -f "$output" ]]; then
    found="$(find "$(dirname "$output")" -maxdepth 1 -type f -name '*selfbuild*.img' | head -1 || true)"
    if [[ -n "$found" ]]; then
        mv "$found" "$output"
    fi
fi

echo "rootfs=$output"
"$script_dir/check_rootfs.sh" "$output"
