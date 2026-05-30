#!/usr/bin/env bash
set -euo pipefail

app_dir="$(cd "$(dirname "$0")" && pwd)"
overlay_dir="${STARRY_OVERLAY_DIR:-}"

if [[ -z "$overlay_dir" ]]; then
    echo "ERROR: STARRY_OVERLAY_DIR is required" >&2
    exit 1
fi

command -v aarch64-linux-gnu-gcc >/dev/null 2>&1 || { echo "ERROR: aarch64-linux-gnu-gcc not found" >&2; exit 1; }
command -v readelf >/dev/null 2>&1 || { echo "ERROR: readelf not found" >&2; exit 1; }

aarch64-linux-gnu-gcc -o "$app_dir/glibc-test" "$app_dir/glibc-test.c"
aarch64-linux-gnu-gcc -o "$app_dir/proc-self-exe-test" "$app_dir/proc-self-exe-test.c"

INTERP=$(readelf -l "$app_dir/glibc-test" | sed -n 's/.*Requesting program interpreter: \(.*\)]/\1/p')
echo "INTERP path: $INTERP"
[[ -n "$INTERP" ]] || { echo "ERROR: no PT_INTERP found" >&2; exit 1; }

SYSROOT=$(aarch64-linux-gnu-gcc -print-sysroot)
LIBC=$(aarch64-linux-gnu-gcc -print-file-name=libc.so.6)
LD_LINUX="$SYSROOT$INTERP"

if [[ ! -f "$LD_LINUX" ]]; then
    LD_LINUX="$(dirname "$LIBC")/$(basename "$INTERP")"
fi

[[ -f "$LD_LINUX" ]] || { echo "ERROR: ld-linux not found" >&2; exit 1; }
[[ -f "$LIBC" ]] || { echo "ERROR: libc.so.6 not found at $LIBC" >&2; exit 1; }

install -Dm0755 "$app_dir/glibc-test" "$overlay_dir/usr/bin/glibc-test"
install -Dm0755 "$app_dir/proc-self-exe-test" "$overlay_dir/usr/bin/proc-self-exe-test"
install -Dm0755 "$app_dir/glibc-test.sh" "$overlay_dir/usr/bin/glibc-test.sh"
install -Dm0755 "$LD_LINUX" "$overlay_dir/$INTERP"
install -Dm0755 "$LIBC" "$overlay_dir/lib/libc.so.6"
