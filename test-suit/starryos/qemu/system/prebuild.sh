#!/bin/sh
set -eu

case ",${STARRY_GROUPED_C_SUBCASES:-}," in
    *,apk-curl-equivalence,*)
        apk add curl
        test -x "$STARRY_STAGING_ROOT/usr/bin/curl"
        ;;
esac
