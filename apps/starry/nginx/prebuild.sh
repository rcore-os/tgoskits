#!/usr/bin/env bash
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"

if [[ -z "$overlay_dir" ]]; then
    echo "error: STARRY_OVERLAY_DIR is required" >&2
    exit 1
fi

install -Dm0755 "$app_dir/smoke/nginx-smoke-tests.sh" "$overlay_dir/usr/bin/nginx-smoke-tests.sh"
install -Dm0755 "$app_dir/phase/nginx-1-2-lifecycle-tests.sh" "$overlay_dir/usr/bin/nginx-phase12-tests.sh"
install -Dm0755 "$app_dir/phase/nginx-1-3-lifecycle-tests.sh" "$overlay_dir/usr/bin/nginx-phase1-tests.sh"
install -Dm0755 "$app_dir/phase/nginx-2-0-http-basic-tests.sh" "$overlay_dir/usr/bin/nginx-phase2-tests.sh"
install -Dm0755 "$app_dir/debug/nginx-2-0-bad-method-debug.sh" "$overlay_dir/usr/bin/nginx-bad-method-debug.sh"
install -Dm0755 "$app_dir/debug/nginx-2-0-bad-method-matrix.sh" "$overlay_dir/usr/bin/nginx-bad-method-matrix.sh"
install -Dm0755 "$app_dir/nginx-alpine-mirror.sh" "$overlay_dir/usr/bin/nginx-alpine-mirror.sh"
