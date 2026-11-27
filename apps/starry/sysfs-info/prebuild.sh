#!/usr/bin/env bash
set -euo pipefail

# sysfs-info prebuild: cross-compile the doc-grounded sysfs carpet as a static,
# non-PIE musl binary and stage it into the per-app rootfs overlay. Nothing is
# pulled from the network; the carpet only reads /sys at runtime on-target.

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"
arch="${STARRY_ARCH:-}"

if [[ -z "$overlay_dir" ]]; then
    echo "error: STARRY_OVERLAY_DIR is required" >&2
    exit 1
fi
if [[ -z "$arch" ]]; then
    echo "error: STARRY_ARCH is required" >&2
    exit 1
fi

# Toolchains live under /opt (and riscv under /usr/local); they are not on the
# default PATH, so prepend the known locations before resolving the compiler.
export PATH="/opt/x86_64-linux-musl-cross/bin:/opt/cross/aarch64-linux-musl-cross/bin:/opt/aarch64-linux-musl-cross/bin:/opt/loongarch64-linux-musl-cross/bin:/usr/local/riscv64-linux-musl-cross/bin:$PATH"

case "$arch" in
    x86_64)      cc=x86_64-linux-musl-gcc ;;
    aarch64)     cc=aarch64-linux-musl-gcc ;;
    riscv64)     cc=riscv64-linux-musl-gcc ;;
    loongarch64) cc=loongarch64-linux-musl-gcc ;;
    *)
        echo "error: unsupported arch: $arch" >&2
        exit 1
        ;;
esac

if ! command -v "$cc" >/dev/null 2>&1; then
    echo "error: cross compiler not found on PATH: $cc" >&2
    exit 1
fi

build_dir="$(mktemp -d)"
trap 'rm -rf "$build_dir"' EXIT

# -no-pie is mandatory: riscv64 musl defaults to static-PIE, which produces a
# dynamic-reloc link error against the static CRT; -no-pie forces a plain
# static executable that StarryOS loads directly.
"$cc" \
    -static -no-pie \
    -O2 -Wall -Wextra -Werror \
    "$app_dir/programs/sysfs_carpet.c" \
    -o "$build_dir/sysfs-carpet"

install -Dm0755 "$build_dir/sysfs-carpet" "$overlay_dir/usr/bin/sysfs-carpet"
install -Dm0755 "$app_dir/sysfs-info.sh" "$overlay_dir/usr/bin/sysfs-info.sh"

echo "sysfs-info: staged /usr/bin/sysfs-carpet ($arch, static no-pie)"
