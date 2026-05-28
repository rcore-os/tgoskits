#!/usr/bin/env bash
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"

if [[ -z "$overlay_dir" ]]; then
    echo "error: STARRY_OVERLAY_DIR is required" >&2
    exit 1
fi

install -Dm0755 "$app_dir/smoke/nginx-smoke-tests.sh" "$overlay_dir/usr/bin/nginx-smoke-tests.sh"
install -Dm0755 "$app_dir/nginx-alpine-mirror.sh" "$overlay_dir/usr/bin/nginx-alpine-mirror.sh"
install -Dm0755 "$app_dir/debug/nginx-multiworker-quit-tests.sh" "$overlay_dir/usr/bin/nginx-multiworker-quit-tests.sh"
install -Dm0755 "$app_dir/phase/nginx-1-3-lifecycle-tests.sh" "$overlay_dir/usr/bin/nginx-phase1-tests.sh"
