#!/bin/sh
set -eu

apk add curl
test -x "$STARRY_STAGING_ROOT/usr/bin/curl"

case ",${STARRY_GROUPED_C_SUBCASES:-}," in
    *,oci-runc-basic,*)
        if [ "${STARRY_TEST_ARCH:-}" = x86_64 ]; then
            apk add runc
            test -x "$STARRY_STAGING_ROOT/usr/bin/runc"
        fi
        ;;
esac
