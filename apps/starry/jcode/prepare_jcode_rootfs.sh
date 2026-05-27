#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
workspace="$(cd "$script_dir/../../.." && pwd)"
base_rootfs="$workspace/tmp/axbuild/rootfs/rootfs-x86_64-alpine.img"
output_rootfs="$workspace/tmp/axbuild/rootfs/rootfs-x86_64-jcode.img"

usage() {
    cat <<EOF
Usage: $(basename "$0") [OPTIONS]

Prepare a local rootfs for the StarryOS jcode example.

Options:
  --base-rootfs PATH   Base rootfs image to copy before injection
                       (default: tmp/axbuild/rootfs/rootfs-x86_64-alpine.img)
  --output-rootfs PATH Output rootfs image for the example
                       (default: tmp/axbuild/rootfs/rootfs-x86_64-jcode.img)
  -h, --help           Show this help

Example:
  apps/starry/jcode/prepare_jcode_rootfs.sh
EOF
}

need_cmd() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "error: required command not found: $1" >&2
        exit 1
    fi
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --base-rootfs)
            base_rootfs="$2"
            shift 2
            ;;
        --output-rootfs)
            output_rootfs="$2"
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "error: unknown argument: $1" >&2
            usage >&2
            exit 1
            ;;
    esac
done

base_rootfs="$(cd "$workspace" && realpath -m "$base_rootfs")"
output_rootfs="$(cd "$workspace" && realpath -m "$output_rootfs")"

need_cmd cp
need_cmd debugfs
need_cmd install
need_cmd mktemp
need_cmd stat

if [[ ! -f "$base_rootfs" ]]; then
    if [[ "$base_rootfs" == "$workspace/tmp/axbuild/rootfs/rootfs-x86_64-alpine.img" ]]; then
        echo "Base rootfs not found; preparing the default x86_64 Alpine rootfs..."
        (cd "$workspace" && cargo xtask starry rootfs --arch x86_64)
    fi
fi

if [[ ! -f "$base_rootfs" ]]; then
    echo "error: base rootfs not found: $base_rootfs" >&2
    exit 1
fi

# Prepare assets (downloads, patches, and builds jcode).
"$script_dir/prepare_jcode_assets.sh"

asset_dir="$workspace/target/jcode/assets"
for f in jcode.bin libglibc_stub.so jcode; do
    if [[ ! -f "$asset_dir/$f" ]]; then
        echo "error: jcode asset missing after prepare_jcode_assets.sh: $f" >&2
        exit 1
    fi
done

mkdir -p "$(dirname "$output_rootfs")"
tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/starry-jcode-rootfs.XXXXXX")"
cleanup() {
    rm -rf "$tmp_dir"
}
trap cleanup EXIT

# Build overlay tree.
overlay="$tmp_dir/overlay"
mkdir -p "$overlay/usr/lib/jcode" "$overlay/usr/bin"

install -m 0755 "$asset_dir/jcode.bin" "$overlay/usr/lib/jcode/jcode.bin"
install -m 0755 "$asset_dir/libglibc_stub.so" "$overlay/usr/lib/jcode/libglibc_stub.so"
install -m 0755 "$asset_dir/jcode" "$overlay/usr/bin/jcode"

# Copy SSL libs.
for f in "$asset_dir"/libssl.so* "$asset_dir"/libcrypto.so*; do
    [[ -f "$f" ]] && install -m 0755 "$f" "$overlay/usr/lib/jcode/"
done

# Copy Kerberos dependency libs.
for lib in libcom_err.so.2 libgssapi_krb5.so.2 libk5crypto.so.3 \
           libkeyutils.so.1 libkrb5.so.3 libkrb5support.so.0; do
    [[ -f "$asset_dir/$lib" ]] && install -m 0755 "$asset_dir/$lib" "$overlay/usr/lib/"
done

# Copy base rootfs and inject overlay.
tmp_rootfs="$tmp_dir/rootfs.img"
cp --reflink=auto "$base_rootfs" "$tmp_rootfs" 2>/dev/null || cp "$base_rootfs" "$tmp_rootfs"

debugfs_script="$tmp_dir/inject.debugfs"
{
    find "$overlay" -type d | sort | while IFS= read -r dir; do
        rel="${dir#"$overlay"}"
        [[ -z "$rel" ]] && continue
        printf 'mkdir %s\n' "$rel"
    done
    find "$overlay" -type f | sort | while IFS= read -r file; do
        rel="${file#"$overlay"}"
        mode="$(stat -c '%a' "$file")"
        printf 'rm %s\n' "$rel"
        printf 'write %s %s\n' "$file" "$rel"
        printf 'sif %s mode 0100%s\n' "$rel" "$mode"
    done
    printf 'quit\n'
} > "$debugfs_script"

debugfs_log="$tmp_dir/debugfs.log"
if ! debugfs -w -f "$debugfs_script" "$tmp_rootfs" >"$debugfs_log" 2>&1; then
    cat "$debugfs_log" >&2
    exit 1
fi
mv "$tmp_rootfs" "$output_rootfs"

display_rootfs="$output_rootfs"
case "$display_rootfs" in
    "$workspace"/*)
        display_rootfs="${display_rootfs#"$workspace"/}"
        ;;
esac

echo "jcode example rootfs ready:"
echo "  $output_rootfs"
echo
echo "Run the offline smoke test with:"
echo "  cargo xtask starry qemu --arch x86_64 \\"
echo "    --qemu-config apps/starry/jcode/qemu-x86_64.toml \\"
echo "    --rootfs $display_rootfs"
echo
echo "For interactive use:"
echo "  cargo xtask starry qemu --arch x86_64 \\"
echo "    --rootfs $display_rootfs"
