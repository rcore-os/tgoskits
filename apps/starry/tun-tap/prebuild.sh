#!/usr/bin/env bash
# prebuild.sh - provision the StarryOS tun-tap carpet.
#
# Cross-compiles the self-contained tun-echo probe to a static musl binary for
# the target arch and stages it plus the driver script into the overlay. No
# runtime network or package fetch is needed: the probe exercises the in-kernel
# /dev/net/tun datapath end to end.
#
# Env from the app runner: STARRY_ARCH, STARRY_OVERLAY_DIR, STARRY_APP_DIR.
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
overlay_dir="${STARRY_OVERLAY_DIR:?prebuild: STARRY_OVERLAY_DIR required}"
arch="${STARRY_ARCH:?prebuild: STARRY_ARCH required}"

# Candidate static-musl cross compilers per arch. The loongarch64 toolchain
# lives outside PATH on the build host, so its directory is searched too.
case "$arch" in
    x86_64)
        compilers="x86_64-linux-musl-gcc musl-gcc"
        extra_path=""
        ;;
    aarch64)
        compilers="aarch64-linux-musl-gcc"
        extra_path="/opt/cross/aarch64-linux-musl-cross/bin"
        ;;
    riscv64)
        compilers="riscv64-linux-musl-gcc"
        extra_path="/usr/local/riscv64-linux-musl-cross/bin"
        ;;
    loongarch64)
        compilers="loongarch64-linux-musl-gcc"
        extra_path="/opt/loongarch64-linux-musl-cross/bin"
        ;;
    *)
        echo "prebuild: unsupported arch: $arch" >&2
        exit 1
        ;;
esac

[[ -n "$extra_path" ]] && export PATH="$extra_path:$PATH"

cc=""
for candidate in $compilers; do
    if command -v "$candidate" >/dev/null 2>&1; then
        cc="$candidate"
        break
    fi
done
if [[ -z "$cc" ]]; then
    echo "prebuild: no static-musl C compiler found for $arch (tried: $compilers)" >&2
    exit 1
fi
echo "prebuild: arch=$arch cc=$cc"

build_dir="$(mktemp -d)"
trap 'rm -rf "$build_dir"' EXIT

compile() {
    local src="$1" dst="$2"
    "$cc" -std=c11 -O2 -Wall -Wextra -static "$app_dir/programs/$src" -o "$build_dir/$dst"
}

compile tun-echo.c tun-echo
compile tap-carpet.c tap-carpet

install -Dm0755 "$build_dir/tun-echo"    "$overlay_dir/usr/bin/tun-echo"
install -Dm0755 "$build_dir/tap-carpet"  "$overlay_dir/usr/bin/tap-carpet"
install -Dm0755 "$app_dir/programs/run-tun-tap.sh" "$overlay_dir/usr/bin/run-tun-tap.sh"

echo "prebuild: staged tun-echo + tap-carpet + run-tun-tap.sh -> overlay for $arch"
