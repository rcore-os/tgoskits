#!/bin/sh
set -eu

apk add lua5.4 lua5.4-cjson

test -x "$STARRY_STAGING_ROOT/usr/bin/lua5.4"
test -f "$STARRY_STAGING_ROOT/usr/lib/lua/5.4/cjson.so"
