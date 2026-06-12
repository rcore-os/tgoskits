#!/bin/sh

set -eu

APACHE_APK_MIRROR_TIMEOUT_SEC="${APACHE_APK_MIRROR_TIMEOUT_SEC:-45}"
APACHE_APK_REPO_FILE="/tmp/apache-apk-repositories"
APACHE_APK_ATTEMPT_LOG="/tmp/apache-apk-attempt.log"

apache_write_repo_file() {
    mirror="$1"
    cat >"$APACHE_APK_REPO_FILE" <<EOF
$mirror/main
$mirror/community
EOF
}

apache_run_apk_add() {
    mirror="$1"
    shift
    packages="$*"

    echo "APACHE_APK_MIRROR_TRY: $mirror"
    apache_write_repo_file "$mirror"

    if timeout "$APACHE_APK_MIRROR_TIMEOUT_SEC" apk --no-progress --update-cache --repositories-file "$APACHE_APK_REPO_FILE" add $packages >"$APACHE_APK_ATTEMPT_LOG" 2>&1; then
        echo "APACHE_APK_MIRROR_OK: $mirror"
        return 0
    else
        rc=$?
    fi

    if [ "$rc" -eq 124 ]; then
        echo "APACHE_APK_MIRROR_FAIL: $mirror (timeout ${APACHE_APK_MIRROR_TIMEOUT_SEC}s)"
    else
        echo "APACHE_APK_MIRROR_FAIL: $mirror (apk rc=$rc)"
    fi
    sed -n '1,120p' "$APACHE_APK_ATTEMPT_LOG" || true
    return 1
}

apache_apk_add_with_fallback() {
    mirror_cn="https://mirrors.tuna.tsinghua.edu.cn/alpine/latest-stable"
    mirror_global="https://dl-cdn.alpinelinux.org/alpine/latest-stable"

    if apache_run_apk_add "$mirror_cn" "$@"; then
        return 0
    fi
    if apache_run_apk_add "$mirror_global" "$@"; then
        return 0
    fi

    echo "APACHE_APK_ALL_MIRRORS_FAILED"
    return 1
}
