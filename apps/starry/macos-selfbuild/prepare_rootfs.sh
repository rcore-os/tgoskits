#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../../.." && pwd)"

usage() {
    cat <<'USAGE'
Usage:
  apps/starry/macos-selfbuild/prepare_rootfs.sh \
    --base-rootfs tmp/axbuild/rootfs/rootfs-aarch64-hvf-toolchain.img \
    --output-rootfs tmp/axbuild/rootfs/rootfs-aarch64-hvf-selfbuild.img

This script copies a prepared AArch64 StarryOS rootfs and injects the current
TGOSKits source tree as /opt/tgoskits-src.tar. It does not install Rust/Cargo;
the base rootfs must already contain the guest toolchain used by the self-build.
USAGE
}

base_rootfs="$repo_root/tmp/axbuild/rootfs/rootfs-aarch64-hvf-toolchain.img"
output_rootfs="$repo_root/tmp/axbuild/rootfs/rootfs-aarch64-hvf-selfbuild.img"
source_dir="$repo_root"
debugfs="${DEBUGFS:-}"

while [[ "$#" -gt 0 ]]; do
    case "$1" in
        --base-rootfs)
            base_rootfs="$2"
            shift 2
            ;;
        --output-rootfs)
            output_rootfs="$2"
            shift 2
            ;;
        --source)
            source_dir="$2"
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

if [[ ! -f "$base_rootfs" ]]; then
    echo "base rootfs not found: $base_rootfs" >&2
    echo "provide a rootfs that already contains guest Cargo/Rust toolchain files" >&2
    exit 1
fi

if [[ ! -f "$source_dir/Cargo.toml" ]]; then
    echo "source dir does not look like TGOSKits: $source_dir" >&2
    exit 1
fi

mkdir -p "$(dirname "$output_rootfs")" "$repo_root/target/starry-macos-selfbuild"
cp "$base_rootfs" "$output_rootfs"

src_tar="$repo_root/target/starry-macos-selfbuild/tgoskits-src.tar"
tar -C "$source_dir" \
    --exclude .git \
    --exclude target \
    --exclude tmp \
    --exclude .cache \
    --exclude .idea \
    --exclude .vscode \
    -cf "$src_tar" .

debugfs_cmd="$repo_root/target/starry-macos-selfbuild/debugfs-prepare-rootfs.cmd"
cat >"$debugfs_cmd" <<EOF
mkdir /opt
rm /opt/tgoskits-src.tar
write $src_tar /opt/tgoskits-src.tar
EOF

"$debugfs" -w -f "$debugfs_cmd" "$output_rootfs" >/dev/null

echo "rootfs=$output_rootfs"
echo "source_tar=/opt/tgoskits-src.tar"
"$script_dir/check_rootfs.sh" "$output_rootfs" || {
    echo "warning: rootfs source was injected, but guest toolchain checks failed" >&2
    echo "install or inject Cargo/Rust wrappers before running self-build" >&2
    exit 1
}
