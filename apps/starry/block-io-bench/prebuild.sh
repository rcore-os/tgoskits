#!/usr/bin/env bash
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"

if [[ -z "$overlay_dir" ]]; then
    echo "error: STARRY_OVERLAY_DIR is required" >&2
    exit 1
fi

case "$STARRY_ARCH" in
    x86_64)
        compilers=(
            x86_64-linux-musl-gcc
            musl-gcc
            x86_64-linux-gnu-gcc
            gcc
        )
        ;;
    *)
        echo "ERROR: unsupported arch for block-io-bench app: $STARRY_ARCH" >&2
        exit 1
        ;;
esac

cc=""
for candidate in "${compilers[@]}"; do
    if command -v "$candidate" >/dev/null 2>&1; then
        cc="$candidate"
        break
    fi
done

if [[ -z "$cc" ]]; then
    echo "ERROR: no C compiler found for $STARRY_ARCH" >&2
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
    -static \
    "$app_dir/block-io-bench.c" \
    -o "$build_dir/block-io-bench"

install -Dm0755 "$build_dir/block-io-bench" "$overlay_dir/usr/bin/block-io-bench"
install -Dm0755 "$app_dir/block-io-bench.sh" "$overlay_dir/usr/bin/block-io-bench.sh"
