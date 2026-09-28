#!/usr/bin/env bash

set -euo pipefail

: "${STARRY_APP_DIR:?STARRY_APP_DIR is required}"
: "${STARRY_OVERLAY_DIR:?STARRY_OVERLAY_DIR is required}"
: "${STARRY_ARCH:?STARRY_ARCH is required}"

case "$STARRY_ARCH" in
    x86_64) compiler_triple=x86_64-linux-musl ;;
    aarch64) compiler_triple=aarch64-linux-musl ;;
    riscv64) compiler_triple=riscv64-linux-musl ;;
    loongarch64) compiler_triple=loongarch64-linux-musl ;;
    *)
        echo "ltp-hackbench does not support architecture: $STARRY_ARCH" >&2
        exit 1
        ;;
esac

if [[ -n "${LTP_HACKBENCH_CC:-}" ]]; then
    compiler="$LTP_HACKBENCH_CC"
elif command -v "${compiler_triple}-gcc" >/dev/null 2>&1; then
    compiler=$(command -v "${compiler_triple}-gcc")
elif [[ -x "/opt/${compiler_triple}-cross/bin/${compiler_triple}-gcc" ]]; then
    compiler="/opt/${compiler_triple}-cross/bin/${compiler_triple}-gcc"
else
    echo "no $compiler_triple compiler found; set LTP_HACKBENCH_CC" >&2
    exit 1
fi

install_dir="$STARRY_OVERLAY_DIR/usr/bin"
mkdir -p "$install_dir"

"$compiler" \
    -std=c11 -O2 -Wall -Wextra -Werror -static -no-pie \
    "$STARRY_APP_DIR/affinity_exec.c" \
    -o "$install_dir/ltp-hackbench-affinity"
install -m 0755 "$STARRY_APP_DIR/ltp-hackbench.sh" \
    "$install_dir/ltp-hackbench-run"

echo "Installed the Starry affinity helper and hackbench runner."
