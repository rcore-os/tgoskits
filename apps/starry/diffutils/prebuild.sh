#!/usr/bin/env bash
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"

if [[ -z "$overlay_dir" ]]; then
    echo "error: STARRY_OVERLAY_DIR is required" >&2
    exit 1
fi

mkdir -p "$overlay_dir/usr/bin"
cp "$app_dir/diffutils-tests.sh" "$overlay_dir/usr/bin/diffutils-tests.sh"
chmod 0755 "$overlay_dir/usr/bin/diffutils-tests.sh"
