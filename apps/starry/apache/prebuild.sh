#!/usr/bin/env bash
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"

if [[ -z "$overlay_dir" ]]; then
    echo "error: STARRY_OVERLAY_DIR is required" >&2
    exit 1
fi

install -Dm0755 "$app_dir/runner/apache-runner.sh" "$overlay_dir/usr/bin/apache-runner.sh"
install -Dm0755 "$app_dir/runner/apache-runner-lib.sh" "$overlay_dir/usr/bin/apache-runner-lib.sh"
install -Dm0755 "$app_dir/smoke/apache-smoke-tests.sh" "$overlay_dir/usr/bin/apache-smoke-tests.sh"
install -Dm0755 "$app_dir/phase/apache-2-0-mpm-prefork-tests.sh" "$overlay_dir/usr/bin/apache-phase20-tests.sh"
install -Dm0755 "$app_dir/phase/apache-3-0-http-static-tests.sh" "$overlay_dir/usr/bin/apache-phase30-tests.sh"
install -Dm0755 "$app_dir/phase/apache-4-0-directory-access-tests.sh" "$overlay_dir/usr/bin/apache-phase40-tests.sh"
install -Dm0755 "$app_dir/phase/apache-5-0-log-lifecycle-tests.sh" "$overlay_dir/usr/bin/apache-phase50-tests.sh"
install -Dm0755 "$app_dir/phase/apache-5-5-sendfile-range-tests.sh" "$overlay_dir/usr/bin/apache-phase55-tests.sh"
install -Dm0755 "$app_dir/phase/apache-7-0-cgi-tests.sh" "$overlay_dir/usr/bin/apache-phase70-tests.sh"
install -Dm0755 "$app_dir/phase/apache-8-0-module-feature-tests.sh" "$overlay_dir/usr/bin/apache-phase80-tests.sh"
install -Dm0755 "$app_dir/debug/apache-mpm-prefork-wait.sh" "$overlay_dir/usr/bin/apache-mpm-prefork-wait.sh"
install -Dm0755 "$app_dir/debug/apache-phase20-restart.sh" "$overlay_dir/usr/bin/apache-phase20-restart.sh"
install -Dm0755 "$app_dir/debug/apache-mpm-thread-futex.sh" "$overlay_dir/usr/bin/apache-mpm-thread-futex.sh"
install -Dm0755 "$app_dir/debug/apache-accept-mutex.sh" "$overlay_dir/usr/bin/apache-accept-mutex.sh"
install -Dm0755 "$app_dir/debug/apache-htaccess-pathwalk.sh" "$overlay_dir/usr/bin/apache-htaccess-pathwalk.sh"
install -Dm0755 "$app_dir/debug/apache-sendfile-mmap-range.sh" "$overlay_dir/usr/bin/apache-sendfile-mmap-range.sh"
install -Dm0755 "$app_dir/debug/apache-graceful-signal.sh" "$overlay_dir/usr/bin/apache-graceful-signal.sh"
install -Dm0755 "$app_dir/debug/apache-cgi-pipe-exec.sh" "$overlay_dir/usr/bin/apache-cgi-pipe-exec.sh"
install -Dm0755 "$app_dir/debug/apache-log-append-reopen.sh" "$overlay_dir/usr/bin/apache-log-append-reopen.sh"
install -Dm0755 "$app_dir/runner/apache-alpine-mirror.sh" "$overlay_dir/usr/bin/apache-alpine-mirror.sh"
