#!/bin/sh
set -eu

apk add gdb

test -x "$STARRY_STAGING_ROOT/usr/bin/gdb"
