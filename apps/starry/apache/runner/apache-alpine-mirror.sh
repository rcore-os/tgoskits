#!/bin/sh

set -eu

APACHE_APK_MIRROR_TIMEOUT_SEC="${APACHE_APK_MIRROR_TIMEOUT_SEC:-120}"
APACHE_APK_MIRROR_RETRIES="${APACHE_APK_MIRROR_RETRIES:-2}"
APACHE_APK_REPO_FILE="/tmp/apache-apk-repositories"
APACHE_APK_ATTEMPT_LOG="/tmp/apache-apk-attempt.log"

# Resolve the Alpine release branch that matches the running rootfs, e.g. "v3.23".
# Pulling packages from a fixed branch keeps the musl/ABI in sync with the
# rootfs base; the moving "latest-stable" alias can drift to a newer Alpine
# release whose packages need a newer musl and then fail to relocate or segfault.
apache_apk_branch() {
    if [ -n "${APACHE_APK_BRANCH:-}" ]; then
        printf '%s\n' "$APACHE_APK_BRANCH"
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
        printf 'latest-stable\n'
    fi
}

apache_write_repo_file() {
    mirror="$1"
    branch="$2"
    cat >"$APACHE_APK_REPO_FILE" <<EOF
$mirror/$branch/main
$mirror/$branch/community
EOF
}

apache_run_apk_add() {
    mirror="$1"
    shift
    packages="$*"

    branch=$(apache_apk_branch)
    attempt=1
    while [ "$attempt" -le "$APACHE_APK_MIRROR_RETRIES" ]; do
        echo "APACHE_APK_MIRROR_TRY: $mirror/$branch attempt=$attempt"
        apache_write_repo_file "$mirror" "$branch"

        if timeout "$APACHE_APK_MIRROR_TIMEOUT_SEC" apk --no-progress --update-cache --repositories-file "$APACHE_APK_REPO_FILE" add $packages >"$APACHE_APK_ATTEMPT_LOG" 2>&1; then
            echo "APACHE_APK_MIRROR_OK: $mirror/$branch"
            return 0
        else
            rc=$?
        fi

        if [ "$rc" -eq 124 ] || [ "$rc" -eq 143 ]; then
            echo "APACHE_APK_MIRROR_FAIL: $mirror/$branch (timeout ${APACHE_APK_MIRROR_TIMEOUT_SEC}s, rc=$rc)"
        else
            echo "APACHE_APK_MIRROR_FAIL: $mirror/$branch (apk rc=$rc)"
        fi
        sed -n '1,120p' "$APACHE_APK_ATTEMPT_LOG" || true
        attempt=$((attempt + 1))
    done
    return 1
}

apache_apk_add_with_fallback() {
    mirror_cn="https://mirrors.tuna.tsinghua.edu.cn/alpine"
    mirror_global="https://dl-cdn.alpinelinux.org/alpine"

    if apache_run_apk_add "$mirror_cn" "$@"; then
        return 0
    fi
    if apache_run_apk_add "$mirror_global" "$@"; then
        return 0
    fi

    echo "APACHE_APK_ALL_MIRRORS_FAILED"
    return 1
}
