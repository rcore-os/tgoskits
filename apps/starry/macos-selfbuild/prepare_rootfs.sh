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
copy_image "$base_rootfs" "$output_rootfs"

git_value() {
    local fallback="$1"
    shift
    git -C "$source_dir" "$@" 2>/dev/null || printf '%s\n' "$fallback"
}

actual_commit="$(git_value unknown rev-parse HEAD)"
if [[ -n "${TGOSKITS_COMMIT:-}" && "$actual_commit" != "unknown" && "$TGOSKITS_COMMIT" != "$actual_commit" ]]; then
    echo "TGOSKITS_COMMIT=$TGOSKITS_COMMIT does not match source HEAD $actual_commit" >&2
    exit 1
fi

source_commit="${TGOSKITS_COMMIT:-$actual_commit}"
source_ref="${TGOSKITS_REF:-$(git_value detached symbolic-ref --quiet --short HEAD)}"
dirty="unknown"
if git -C "$source_dir" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    if [[ -n "$(git -C "$source_dir" status --porcelain --untracked-files=all)" ]]; then
        dirty="true"
    else
        dirty="false"
    fi
fi

meta_file="$repo_root/target/starry-macos-selfbuild/tgoskits-src.meta"
cat >"$meta_file" <<EOF
commit=$source_commit
ref=$source_ref
dirty=$dirty
generated_by=apps/starry/macos-selfbuild/prepare_rootfs.sh
EOF

meta_in_tar="$repo_root/target/starry-macos-selfbuild/.tgoskits-source-meta"
cp "$meta_file" "$meta_in_tar"

src_tar="$repo_root/target/starry-macos-selfbuild/tgoskits-src.tar"
tar -C "$source_dir" \
    --exclude .git \
    --exclude target \
    --exclude tmp \
    --exclude .cache \
    --exclude .idea \
    --exclude .vscode \
    -cf "$src_tar" .
tar -C "$repo_root/target/starry-macos-selfbuild" -rf "$src_tar" .tgoskits-source-meta

debugfs_cmd="$repo_root/target/starry-macos-selfbuild/debugfs-prepare-rootfs.cmd"
debugfs_log="$repo_root/target/starry-macos-selfbuild/debugfs-prepare-rootfs.log"
cat >"$debugfs_cmd" <<EOF
mkdir /opt
rm /opt/tgoskits-src.tar
rm /opt/tgoskits-src.meta
write $src_tar /opt/tgoskits-src.tar
write $meta_file /opt/tgoskits-src.meta
EOF

if ! "$debugfs" -w -f "$debugfs_cmd" "$output_rootfs" >"$debugfs_log" 2>&1; then
    cat "$debugfs_log" >&2
    exit 1
fi

echo "rootfs=$output_rootfs"
echo "source_tar=/opt/tgoskits-src.tar"
echo "source_commit=$source_commit"
echo "source_ref=$source_ref"
echo "source_dirty=$dirty"
"$script_dir/check_rootfs.sh" "$output_rootfs" || {
    echo "warning: rootfs source was injected, but guest toolchain checks failed" >&2
    echo "install or inject Cargo/Rust wrappers before running self-build" >&2
    exit 1
}
