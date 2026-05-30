#!/usr/bin/env bash
set -euo pipefail

app_dir="$(cd "$(dirname "$0")" && pwd)"
overlay_dir="${STARRY_OVERLAY_DIR:-}"

if [[ -z "$overlay_dir" ]]; then
    echo "ERROR: STARRY_OVERLAY_DIR is required" >&2
    exit 1
fi

case "$STARRY_ARCH" in
    aarch64)
        GCC_PREFIX="aarch64-linux-gnu"
        ;;
    riscv64)
        GCC_PREFIX="riscv64-linux-gnu"
        ;;
    x86_64)
        GCC_PREFIX="x86_64-linux-gnu"
        ;;
    *)
        echo "ERROR: unsupported arch: $STARRY_ARCH" >&2
        exit 1
        ;;
esac

command -v "${GCC_PREFIX}-gcc" >/dev/null 2>&1 || { echo "ERROR: ${GCC_PREFIX}-gcc not found" >&2; exit 1; }
command -v readelf >/dev/null 2>&1 || { echo "ERROR: readelf not found" >&2; exit 1; }

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
