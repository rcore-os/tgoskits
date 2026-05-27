#!/usr/bin/env bash
set -euo pipefail

workspace="${STARRY_WORKSPACE:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)}"
app_dir="${STARRY_APP_DIR:-$workspace/apps/starry/pip}"
arch="${STARRY_ARCH:-x86_64}"
base_rootfs="${STARRY_BASE_ROOTFS:-}"
staging_root="${STARRY_STAGING_ROOT:-}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"

if [[ -z "$base_rootfs" || -z "$staging_root" || -z "$overlay_dir" ]]; then
    echo "error: STARRY_BASE_ROOTFS, STARRY_STAGING_ROOT, and STARRY_OVERLAY_DIR are required" >&2
    exit 1
fi

# Map arch to qemu-user binary
case "$arch" in
    aarch64)     qemu_user="qemu-aarch64-static" ;;
    riscv64)     qemu_user="qemu-riscv64-static" ;;
    x86_64)      qemu_user="qemu-x86_64-static" ;;
    loongarch64) qemu_user="qemu-loongarch64-static" ;;
    *)           echo "error: unsupported arch: $arch" >&2; exit 1 ;;
esac

if ! command -v "$qemu_user" >/dev/null 2>&1; then
    echo "error: $qemu_user not found on host" >&2
    exit 1
fi

if ! command -v debugfs >/dev/null 2>&1; then
    echo "error: debugfs not found on host (install e2fsprogs)" >&2
    exit 1
fi

echo "[pip prebuild] extracting rootfs into staging..."
debugfs -R "rdump / $staging_root" "$base_rootfs"

# Copy host resolv.conf so apk can resolve DNS
cp /etc/resolv.conf "$staging_root/etc/resolv.conf"

# Create apk cache dir so apk add works
mkdir -p "$staging_root/etc/apk"
echo "https://dl-cdn.alpinelinux.org/alpine/v3.21/main" > "$staging_root/etc/apk/repositories"
echo "https://dl-cdn.alpinelinux.org/alpine/v3.21/community" >> "$staging_root/etc/apk/repositories"

# Set up musl loader search path
for loader_path in "$staging_root"/lib/ld-musl-*.so.1; do
    if [[ -f "$loader_path" ]]; then
        loader_name="$(basename "$loader_path")"
        arch_name="${loader_name#ld-musl-}"
        arch_name="${arch_name%.so.1}"
        printf '/usr/lib\n/lib\n' > "$staging_root/etc/$arch_name.path" 2>/dev/null || true
        break
    fi
done

echo "[pip prebuild] installing python3 and py3-pip via apk..."
# Run apk directly via qemu-user; going through busybox sh fails because
# the kernel can't resolve the guest's ELF interpreter for nested exec calls.
export QEMU_LD_PREFIX="$staging_root"
export LD_LIBRARY_PATH="$staging_root/usr/lib:$staging_root/lib"
"$qemu_user" -L "$staging_root" "$staging_root/sbin/apk" add --no-cache --root "$staging_root" python3 py3-pip

echo "[pip prebuild] copying python+pip to overlay..."
mkdir -p "$overlay_dir/usr/bin" "$overlay_dir/usr/lib" "$overlay_dir/lib"

# Copy Python and pip binaries (preserving symlinks initially)
for d in usr/bin usr/lib lib; do
    src="$staging_root/$d"
    dst="$overlay_dir/$d"
    if [[ -d "$src" ]]; then
        cp -a "$src/." "$dst/"
    fi
done

# Resolve symlinks in the overlay: replace each symlink with a copy of its
# target, resolving paths relative to the staging root (not the host).
# Loop until no symlinks remain (handles chains like a -> b -> c).
while symlinks="$(find "$overlay_dir" -type l)" && [[ -n "$symlinks" ]]; do
    echo "$symlinks" | while IFS= read -r link; do
        [[ -z "$link" ]] && continue
        target="$(readlink "$link")"
        if [[ "$target" = /* ]]; then
            resolved="$staging_root$target"
        else
            resolved="$(dirname "$link")/$target"
        fi
        if [[ -e "$resolved" ]]; then
            rm "$link"
            cp -a "$resolved" "$link"
        else
            echo "[pip prebuild] warning: dangling symlink $link -> $target, removing" >&2
            rm "$link"
        fi
    done
done

# Copy test script to overlay
install -Dm0755 "$app_dir/test_pip.sh" "$overlay_dir/usr/bin/test_pip.sh"

echo "[pip prebuild] done."
