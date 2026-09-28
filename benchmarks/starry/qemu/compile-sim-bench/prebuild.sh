#!/usr/bin/env bash

set -euo pipefail

: "${STARRY_APP_DIR:?STARRY_APP_DIR is required}"
: "${STARRY_OVERLAY_DIR:?STARRY_OVERLAY_DIR is required}"
: "${STARRY_ARCH:?STARRY_ARCH is required}"

if [[ "$STARRY_ARCH" != x86_64 ]]; then
    echo "compile-sim-bench currently supports only x86_64" >&2
    exit 1
fi

compiler_triple=x86_64-linux-musl
if [[ -n "${COMPILE_SIM_CC:-}" ]]; then
    compiler="$COMPILE_SIM_CC"
elif command -v "${compiler_triple}-gcc" >/dev/null 2>&1; then
    compiler=$(command -v "${compiler_triple}-gcc")
elif [[ -x "/opt/${compiler_triple}-cross/bin/${compiler_triple}-gcc" ]]; then
    compiler="/opt/${compiler_triple}-cross/bin/${compiler_triple}-gcc"
else
    echo "no $compiler_triple compiler found; set COMPILE_SIM_CC" >&2
    exit 1
fi

install_dir="$STARRY_OVERLAY_DIR/usr/bin"
ltp_app_dir="$STARRY_APP_DIR/../ltp-hackbench"
mkdir -p "$install_dir"

"$compiler" \
    -std=c11 -O2 -Wall -Wextra -Werror -static -no-pie \
    "$STARRY_APP_DIR/compile-sim-bench.c" \
    -o "$install_dir/compile-sim-bench"
install -m 0755 "$STARRY_APP_DIR/compile-sim-bench-run.sh" \
    "$install_dir/compile-sim-bench-run"

# Install the existing unmodified-LTP wrapper as well, so Starry and Linux can
# boot the same generated rootfs for both benchmark families.
"$compiler" \
    -std=c11 -O2 -Wall -Wextra -Werror -static -no-pie \
    "$ltp_app_dir/affinity_exec.c" \
    -o "$install_dir/ltp-hackbench-affinity"
install -m 0755 "$ltp_app_dir/ltp-hackbench.sh" \
    "$install_dir/ltp-hackbench-run"
install -m 0755 "$STARRY_APP_DIR/linux-compile-sim-init.sh" \
    "$install_dir/linux-compile-sim-init"
install -m 0755 "$STARRY_APP_DIR/linux-ltp-hackbench-init.sh" \
    "$install_dir/linux-ltp-hackbench-init"

echo "Installed compile-sim and the shared Linux/Starry benchmark launchers."
