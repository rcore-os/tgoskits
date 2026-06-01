#!/usr/bin/env bash
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"

if [[ -z "$overlay_dir" ]]; then
    echo "error: STARRY_OVERLAY_DIR is required" >&2
    exit 1
fi

install -Dm0755 "$app_dir/static-pie-test.sh" "$overlay_dir/usr/bin/static-pie-test.sh"

# Compile and copy static-pie test binary
TOOLCHAIN="/root/project/toolchains/riscv64-linux-musl-cross/bin/riscv64-linux-musl-gcc"
TEST_SRC="/tmp/opencode/static-pie-test.c"
TEST_BIN="/tmp/opencode/static-pie-test"

mkdir -p /tmp/opencode
cat > "$TEST_SRC" << "CEOF"
#include <stdio.h>
int main(void) {
    printf("static-pie test OK\n");
    return 0;
}
CEOF

if [[ -f "$TOOLCHAIN" ]]; then
    "$TOOLCHAIN" -static -o "$TEST_BIN" "$TEST_SRC"
    install -Dm0755 "$TEST_BIN" "$overlay_dir/usr/bin/static-pie-test"
    echo "Static-pie binary compiled and installed to /usr/bin/static-pie-test"
else
    echo "Error: riscv64 toolchain not found" >&2
    exit 1
fi
