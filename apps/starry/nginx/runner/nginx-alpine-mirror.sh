#!/bin/sh

set -eu

NGINX_APK_MIRROR_TIMEOUT_SEC="${NGINX_APK_MIRROR_TIMEOUT_SEC:-120}"
NGINX_APK_MIRROR_RETRIES="${NGINX_APK_MIRROR_RETRIES:-2}"
NGINX_APK_REPO_FILE="/tmp/nginx-apk-repositories"
NGINX_APK_ATTEMPT_LOG="/tmp/nginx-apk-attempt.log"

# Resolve the Alpine release branch that matches the running rootfs, e.g. "v3.23".
# Pulling packages from a fixed branch keeps the musl/ABI in sync with the
# rootfs base; the moving "latest-stable" alias can drift to a newer Alpine
# release whose packages need a newer musl (e.g. renameat2) and then fail to
# relocate or segfault on this rootfs.
nginx_apk_branch() {
    if [ -n "${NGINX_APK_BRANCH:-}" ]; then
        printf '%s\n' "$NGINX_APK_BRANCH"
        return 0
    fi
    release=""
    if [ -r /etc/alpine-release ]; then
        release=$(cat /etc/alpine-release 2>/dev/null)
    fi
    major=$(printf '%s' "$release" | cut -d. -f1)
    minor=$(printf '%s' "$release" | cut -d. -f2)
    if [ -n "$major" ] && [ -n "$minor" ]; then
        printf 'v%s.%s\n' "$major" "$minor"
    else
        # Fall back to latest-stable only when the release cannot be read.
        printf 'latest-stable\n'
    fi
}

write_repo_file() {
    mirror="$1"
    branch="$2"
    cat >"$NGINX_APK_REPO_FILE" <<EOF
$mirror/$branch/main
$mirror/$branch/community
EOF
}

run_apk_add() {
    mirror="$1"
    shift
    packages="$*"

    branch=$(nginx_apk_branch)
    attempt=1
    while [ "$attempt" -le "$NGINX_APK_MIRROR_RETRIES" ]; do
        echo "NGINX_APK_MIRROR_TRY: $mirror/$branch attempt=$attempt"
        write_repo_file "$mirror" "$branch"

        if timeout "$NGINX_APK_MIRROR_TIMEOUT_SEC" apk --no-progress --update-cache --repositories-file "$NGINX_APK_REPO_FILE" add $packages >"$NGINX_APK_ATTEMPT_LOG" 2>&1; then
            echo "NGINX_APK_MIRROR_OK: $mirror/$branch"
            return 0
        else
            rc=$?
        fi

        if [ "$rc" -eq 124 ] || [ "$rc" -eq 143 ]; then
            echo "NGINX_APK_MIRROR_FAIL: $mirror/$branch (timeout ${NGINX_APK_MIRROR_TIMEOUT_SEC}s, rc=$rc)"
        else
            echo "NGINX_APK_MIRROR_FAIL: $mirror/$branch (apk rc=$rc)"
        fi
        sed -n '1,120p' "$NGINX_APK_ATTEMPT_LOG" || true
        attempt=$((attempt + 1))
    done
    return 1
}

nginx_apk_add_with_fallback() {
    mirror_cn="https://mirrors.tuna.tsinghua.edu.cn/alpine"
    mirror_global="https://dl-cdn.alpinelinux.org/alpine"

    if run_apk_add "$mirror_cn" "$@"; then
        return 0
    fi
    if run_apk_add "$mirror_global" "$@"; then
        return 0
    fi

    echo "NGINX_APK_ALL_MIRRORS_FAILED"
    return 1
}
