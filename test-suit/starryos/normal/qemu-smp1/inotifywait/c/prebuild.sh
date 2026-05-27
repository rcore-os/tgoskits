#!/bin/sh
set -eu

apk add inotify-tools

mkdir -p "$STARRY_CASE_OVERLAY_DIR/usr/bin"
cp "$STARRY_STAGING_ROOT/usr/bin/inotifywait" "$STARRY_CASE_OVERLAY_DIR/usr/bin/inotifywait"
chmod 0755 "$STARRY_CASE_OVERLAY_DIR/usr/bin/inotifywait"
