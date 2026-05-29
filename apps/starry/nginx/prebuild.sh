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
install -Dm0755 "$app_dir/phase/nginx-0-0-env-rlimit-tests.sh" "$overlay_dir/usr/bin/nginx-phase00-tests.sh"
install -Dm0755 "$app_dir/phase/nginx-1-3-lifecycle-tests.sh" "$overlay_dir/usr/bin/nginx-phase1-tests.sh"
install -Dm0755 "$app_dir/phase/nginx-2-0-http-basic-tests.sh" "$overlay_dir/usr/bin/nginx-phase2-tests.sh"
install -Dm0755 "$app_dir/phase/nginx-3-1-short-connection-tests.sh" "$overlay_dir/usr/bin/nginx-phase31-tests.sh"
install -Dm0755 "$app_dir/phase/nginx-3-2-keepalive-tests.sh" "$overlay_dir/usr/bin/nginx-phase32-tests.sh"
install -Dm0755 "$app_dir/phase/nginx-3-3-slow-header-tests.sh" "$overlay_dir/usr/bin/nginx-phase33-tests.sh"
install -Dm0755 "$app_dir/phase/nginx-4-1-sendfile-off-tests.sh" "$overlay_dir/usr/bin/nginx-phase41-tests.sh"
install -Dm0755 "$app_dir/phase/nginx-4-2-sendfile-on-tests.sh" "$overlay_dir/usr/bin/nginx-phase42-tests.sh"
install -Dm0755 "$app_dir/phase/nginx-4-3-range-tests.sh" "$overlay_dir/usr/bin/nginx-phase43-tests.sh"
install -Dm0755 "$app_dir/phase/nginx-5-0-request-body-tests.sh" "$overlay_dir/usr/bin/nginx-phase50-tests.sh"
install -Dm0755 "$app_dir/phase/nginx-6-0-log-fs-tests.sh" "$overlay_dir/usr/bin/nginx-phase60-tests.sh"
install -Dm0755 "$app_dir/phase/nginx-7-0-signal-lifecycle-tests.sh" "$overlay_dir/usr/bin/nginx-phase70-tests.sh"
install -Dm0755 "$app_dir/phase/nginx-9-0-config-feature-tests.sh" "$overlay_dir/usr/bin/nginx-phase90-tests.sh"
install -Dm0755 "$app_dir/debug/nginx-2-0-bad-method-debug.sh" "$overlay_dir/usr/bin/nginx-bad-method-debug.sh"
install -Dm0755 "$app_dir/debug/nginx-2-0-bad-method-matrix.sh" "$overlay_dir/usr/bin/nginx-bad-method-matrix.sh"
install -Dm0755 "$app_dir/debug/nginx-4-2-sendfile-on-debug.sh" "$overlay_dir/usr/bin/nginx-sendfile-on-debug.sh"
install -Dm0755 "$app_dir/nginx-alpine-mirror.sh" "$overlay_dir/usr/bin/nginx-alpine-mirror.sh"
