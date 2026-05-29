#!/bin/sh
set -eu

apk add inotify-tools

test -x "$STARRY_STAGING_ROOT/usr/bin/inotifywait"
