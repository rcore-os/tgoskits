#!/usr/bin/env bash
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"

if [[ -z "$overlay_dir" ]]; then
    echo "error: STARRY_OVERLAY_DIR is required" >&2
    exit 1
fi
if [[ "${STARRY_ARCH:-}" != "x86_64" ]]; then
    echo "error: futex-ping-pong-bench currently supports x86_64 only" >&2
    exit 1
fi

compilers=(x86_64-linux-musl-gcc musl-gcc x86_64-linux-gnu-gcc gcc)
cc=""
for candidate in "${compilers[@]}"; do
    if command -v "$candidate" >/dev/null 2>&1; then
        cc="$candidate"
        break
    fi
done
if [[ -z "$cc" ]]; then
    echo "error: no static x86_64 C compiler found" >&2
    exit 1
fi

build_dir="$(mktemp -d)"
trap 'rm -rf "$build_dir"' EXIT

"$cc" \
    -std=c11 \
    -O2 \
    -Wall \
    -Wextra \
    -Werror \
    -pthread \
    -static \
    "$app_dir/futex-ping-pong-bench.c" \
    -o "$build_dir/futex-ping-pong-bench"

install -Dm0755 \
    "$build_dir/futex-ping-pong-bench" \
    "$overlay_dir/usr/bin/futex-ping-pong-bench"
install -Dm0755 \
    "$app_dir/futex-ping-pong-bench.sh" \
    "$overlay_dir/usr/bin/futex-ping-pong-bench.sh"
