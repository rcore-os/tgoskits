#!/bin/sh

set -eu

NGINX_APK_MIRROR_TIMEOUT_SEC="${NGINX_APK_MIRROR_TIMEOUT_SEC:-120}"
NGINX_APK_MIRROR_RETRIES="${NGINX_APK_MIRROR_RETRIES:-2}"
NGINX_APK_REPO_FILE="/tmp/nginx-apk-repositories"
NGINX_APK_ATTEMPT_LOG="/tmp/nginx-apk-attempt.log"

write_repo_file() {
    mirror="$1"
    cat >"$NGINX_APK_REPO_FILE" <<EOF
$mirror/main
$mirror/community
EOF
}

run_apk_add() {
    mirror="$1"
    shift
    packages="$*"

    attempt=1
    while [ "$attempt" -le "$NGINX_APK_MIRROR_RETRIES" ]; do
        echo "NGINX_APK_MIRROR_TRY: $mirror attempt=$attempt"
        write_repo_file "$mirror"

        if timeout "$NGINX_APK_MIRROR_TIMEOUT_SEC" apk --no-progress --update-cache --repositories-file "$NGINX_APK_REPO_FILE" add $packages >"$NGINX_APK_ATTEMPT_LOG" 2>&1; then
            echo "NGINX_APK_MIRROR_OK: $mirror"
            return 0
        else
            rc=$?
        fi

        if [ "$rc" -eq 124 ] || [ "$rc" -eq 143 ]; then
            echo "NGINX_APK_MIRROR_FAIL: $mirror (timeout ${NGINX_APK_MIRROR_TIMEOUT_SEC}s, rc=$rc)"
        else
            echo "NGINX_APK_MIRROR_FAIL: $mirror (apk rc=$rc)"
        fi
        sed -n '1,120p' "$NGINX_APK_ATTEMPT_LOG" || true
        attempt=$((attempt + 1))
    done
    return 1
}

nginx_apk_add_with_fallback() {
    mirror_cn="https://mirrors.tuna.tsinghua.edu.cn/alpine/latest-stable"
    mirror_global="https://dl-cdn.alpinelinux.org/alpine/latest-stable"

    if run_apk_add "$mirror_cn" "$@"; then
        return 0
    fi
    if run_apk_add "$mirror_global" "$@"; then
        return 0
    fi

    echo "NGINX_APK_ALL_MIRRORS_FAILED"
    return 1
}
