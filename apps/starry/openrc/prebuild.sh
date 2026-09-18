#!/usr/bin/env bash
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"

if [[ -z "$overlay_dir" ]]; then
    echo "error: STARRY_OVERLAY_DIR is required" >&2
    exit 1
fi

workspace_dir="${STARRY_WORKSPACE:-$(cd "$app_dir/../../.." && pwd)}"
install -Dm0755 "$workspace_dir/test-suit/starryos/qemu/openrc/sh/openrc-test.sh" \
    "$overlay_dir/usr/bin/openrc-test.sh"
