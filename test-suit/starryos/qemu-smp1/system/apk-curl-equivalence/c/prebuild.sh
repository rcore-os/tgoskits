#!/bin/sh
set -eu

apk add curl
test -x "$STARRY_STAGING_ROOT/usr/bin/curl"
