#!/usr/bin/env bash
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"

if [[ -z "$overlay_dir" ]]; then
    echo "error: STARRY_OVERLAY_DIR is required" >&2
    exit 1
fi

install -Dm0755 "$app_dir/gcc-test.sh" "$overlay_dir/usr/bin/gcc-test.sh"
install -Dm0644 "$app_dir/c/src/main.c" "$overlay_dir/usr/src/test-gcc/main.c"
