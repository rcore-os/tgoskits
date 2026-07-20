#!/usr/bin/env bash
# prebuild.sh - cross-compile the cpu-concurrency carpet into the per-arch overlay.
#
# The carpet is a single self-contained C11 source linked fully static against musl
# (pthread is part of musl libc), so no runtime library closure is staged - only the
# one binary is installed into /usr/bin. Reproducible: the compiler is resolved from
# the standard musl-cross toolchain names on PATH, then the conventional
# /opt/<triple>-cross install prefix, then `zig cc -target <triple>` as a final
# portable fallback. No pinned URLs, no cached artifacts.
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
overlay_dir="${STARRY_OVERLAY_DIR:?prebuild: STARRY_OVERLAY_DIR is required}"
arch="${STARRY_ARCH:?prebuild: STARRY_ARCH is required}"

case "$arch" in
    x86_64)      triple="x86_64-linux-musl" ;;
    aarch64)     triple="aarch64-linux-musl" ;;
    riscv64)     triple="riscv64-linux-musl" ;;
    loongarch64) triple="loongarch64-linux-musl" ;;
    *) echo "prebuild: unsupported arch: $arch" >&2; exit 1 ;;
esac

build_dir="$(mktemp -d)"
trap 'rm -rf "$build_dir"' EXIT
out="$build_dir/cpu-concurrency"
src="$app_dir/cpu_concurrency.c"
# -no-pie: a plain static (non-PIE) binary. Without it the riscv64 musl toolchain defaults to
# static-PIE, whose dynamic relocations land in a read-only segment and fail to link
# ("read-only segment has dynamic relocations"); -no-pie is correct and harmless on all arches.
cflags=(-std=c11 -O2 -Wall -Wextra -Werror -static -no-pie -pthread)

# 1) standard cross-gcc on PATH
if command -v "${triple}-gcc" >/dev/null 2>&1; then
    "${triple}-gcc" "${cflags[@]}" "$src" -o "$out"
# 2) conventional /opt install prefix (musl.cc toolchains)
elif [[ -x "/opt/${triple}-cross/bin/${triple}-gcc" ]]; then
    "/opt/${triple}-cross/bin/${triple}-gcc" "${cflags[@]}" "$src" -o "$out"
# 3) host gcc only for a native x86_64 build
elif [[ "$arch" == "x86_64" ]] && command -v musl-gcc >/dev/null 2>&1; then
    musl-gcc "${cflags[@]}" "$src" -o "$out"
# 4) portable fallback: zig cc targets every arch from one toolchain
elif command -v zig >/dev/null 2>&1; then
    zig cc -target "$triple" "${cflags[@]}" "$src" -o "$out"
else
    echo "prebuild: no musl cross toolchain for $triple (tried ${triple}-gcc, /opt/${triple}-cross, zig cc)" >&2
    exit 1
fi

install -Dm0755 "$out" "$overlay_dir/usr/bin/cpu-concurrency"
install -Dm0755 "$app_dir/cpu-concurrency.sh" "$overlay_dir/usr/bin/cpu-concurrency.sh"
echo "prebuild: cpu-concurrency built static for $arch ($triple)"
