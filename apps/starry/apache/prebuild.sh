#!/usr/bin/env bash
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"

if [[ -z "$overlay_dir" ]]; then
    echo "error: STARRY_OVERLAY_DIR is required" >&2
    exit 1
fi

install -Dm0755 "$app_dir/smoke/apache-smoke-tests.sh" "$overlay_dir/usr/bin/apache-smoke-tests.sh"
install -Dm0755 "$app_dir/phase/apache-2-0-mpm-prefork-tests.sh" "$overlay_dir/usr/bin/apache-phase20-tests.sh"
install -Dm0755 "$app_dir/phase/apache-3-0-http-static-tests.sh" "$overlay_dir/usr/bin/apache-phase30-tests.sh"
install -Dm0755 "$app_dir/phase/apache-4-0-directory-access-tests.sh" "$overlay_dir/usr/bin/apache-phase40-tests.sh"
install -Dm0755 "$app_dir/phase/apache-5-0-log-lifecycle-tests.sh" "$overlay_dir/usr/bin/apache-phase50-tests.sh"
install -Dm0755 "$app_dir/phase/apache-5-5-sendfile-range-tests.sh" "$overlay_dir/usr/bin/apache-phase55-tests.sh"
install -Dm0755 "$app_dir/phase/apache-7-0-cgi-tests.sh" "$overlay_dir/usr/bin/apache-phase70-tests.sh"
install -Dm0755 "$app_dir/phase/apache-8-0-module-feature-tests.sh" "$overlay_dir/usr/bin/apache-phase80-tests.sh"
install -Dm0755 "$app_dir/apache-alpine-mirror.sh" "$overlay_dir/usr/bin/apache-alpine-mirror.sh"
