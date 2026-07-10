#!/usr/bin/env bash
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"

if [[ -z "$overlay_dir" ]]; then
    echo "ERROR: STARRY_OVERLAY_DIR is required" >&2
    exit 1
fi

case "$STARRY_ARCH" in
    aarch64)
        GCC_PREFIX="aarch64-linux-gnu"
        GCC_PACKAGE="gcc-aarch64-linux-gnu"
        LIBC_DEV_PACKAGE="libc6-dev-arm64-cross"
        ;;
    riscv64)
        GCC_PREFIX="riscv64-linux-gnu"
        GCC_PACKAGE="gcc-riscv64-linux-gnu"
        LIBC_DEV_PACKAGE="libc6-dev-riscv64-cross"
        ;;
    x86_64)
        GCC_PREFIX="x86_64-linux-gnu"
        GCC_PACKAGE="gcc-x86-64-linux-gnu"
        LIBC_DEV_PACKAGE="libc6-dev"
        ;;
    *)
        echo "ERROR: unsupported arch: $STARRY_ARCH" >&2
        exit 1
        ;;
esac

ensure_host_packages() {
    local missing=()

    if ! command -v "${GCC_PREFIX}-gcc" >/dev/null 2>&1; then
        missing+=("$GCC_PACKAGE" "$LIBC_DEV_PACKAGE")
    elif ! printf "#include <stdio.h>\n" | "${GCC_PREFIX}-gcc" -E -x c - >/dev/null 2>&1; then
        missing+=("$LIBC_DEV_PACKAGE")
    fi
    command -v readelf >/dev/null 2>&1 || missing+=(binutils)
    command -v install >/dev/null 2>&1 || missing+=(coreutils)

    if [[ ${#missing[@]} -eq 0 ]]; then
        return
    fi

    if ! command -v apt-get >/dev/null 2>&1; then
        echo "ERROR: missing required host packages and apt-get is unavailable: ${missing[*]}" >&2
        exit 1
    fi

    echo "installing missing host packages: ${missing[*]}"
    apt-get update
    apt-get install -y --no-install-recommends "${missing[@]}"
}

ensure_host_packages

"${GCC_PREFIX}-gcc" -o "$app_dir/glibc-dynamic-smoke" "$app_dir/glibc-dynamic-smoke.c"
"${GCC_PREFIX}-gcc" -o "$app_dir/proc-self-exe-test" "$app_dir/proc-self-exe-test.c"
"${GCC_PREFIX}-gcc" -o "$app_dir/pthread-test" "$app_dir/pthread-test.c" -lpthread
"${GCC_PREFIX}-gcc" -o "$app_dir/regex-test" "$app_dir/regex-test.c"

INTERP=$(readelf -l "$app_dir/glibc-dynamic-smoke" | sed -n 's/.*Requesting program interpreter: \(.*\)]/\1/p')
echo "INTERP path: $INTERP"
[[ -n "$INTERP" ]] || { echo "ERROR: no PT_INTERP found" >&2; exit 1; }

SYSROOT=$("${GCC_PREFIX}-gcc" -print-sysroot)
LIBC=$("${GCC_PREFIX}-gcc" -print-file-name=libc.so.6)
LD_LINUX="$SYSROOT$INTERP"

if [[ ! -f "$LD_LINUX" ]]; then
    LD_LINUX="$(dirname "$LIBC")/$(basename "$INTERP")"
fi

[[ -f "$LD_LINUX" ]] || { echo "ERROR: ld-linux not found" >&2; exit 1; }
[[ -f "$LIBC" ]] || { echo "ERROR: libc.so.6 not found at $LIBC" >&2; exit 1; }

install -Dm0755 "$app_dir/glibc-dynamic-smoke" "$overlay_dir/usr/bin/glibc-dynamic-smoke"
install -Dm0755 "$app_dir/proc-self-exe-test" "$overlay_dir/usr/bin/proc-self-exe-test"
install -Dm0755 "$app_dir/pthread-test" "$overlay_dir/usr/bin/pthread-test"
install -Dm0755 "$app_dir/regex-test" "$overlay_dir/usr/bin/regex-test"
install -Dm0755 "$app_dir/glibc-dynamic-smoke.sh" "$overlay_dir/usr/bin/glibc-dynamic-smoke.sh"
install -Dm0755 "$LD_LINUX" "$overlay_dir/$INTERP"
install -Dm0755 "$LIBC" "$overlay_dir/lib/libc.so.6"
