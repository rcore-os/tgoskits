#!/bin/sh
set -eu

apk add nix
test -x "$STARRY_STAGING_ROOT/usr/bin/nix"
test -f "$STARRY_STAGING_ROOT/etc/nix/nix.conf"
