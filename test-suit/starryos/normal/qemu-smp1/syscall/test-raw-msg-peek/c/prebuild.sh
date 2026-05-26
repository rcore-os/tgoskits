#!/bin/sh
set -eu

if [ -z "${STARRY_STAGING_ROOT:-}" ]; then
    apk add binutils gcc musl-dev
    exit 0
fi

apk add binutils gcc musl-dev
