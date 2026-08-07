#!/bin/sh
set -eu

apk_add_with_retry() {
    attempt=1
    max_attempts=3
    retry_delay_seconds="${STARRY_APK_RETRY_DELAY_SECONDS:-5}"

    while ! apk add "$@"; do
        if [ "$attempt" -ge "$max_attempts" ]; then
            echo "apk add failed after $attempt attempts" >&2
            return 1
        fi

        echo "apk add failed on attempt $attempt; retrying" >&2
        if [ "$retry_delay_seconds" -gt 0 ]; then
            sleep "$((retry_delay_seconds * attempt))"
        fi
        attempt="$((attempt + 1))"
    done
}

case ",${STARRY_GROUPED_C_SUBCASES:-}," in
    *,apk-curl-equivalence,*)
        apk_add_with_retry curl
        test -x "$STARRY_STAGING_ROOT/usr/bin/curl"
        ;;
esac
