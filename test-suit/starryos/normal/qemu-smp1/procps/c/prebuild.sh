#!/bin/sh
set -eu

apk add procps

for tool in ps free uptime pgrep pmap; do
    if [ ! -x "$STARRY_STAGING_ROOT/usr/bin/$tool" ] \
        && [ ! -x "$STARRY_STAGING_ROOT/bin/$tool" ]; then
        echo "missing procps tool: $tool" >&2
        exit 1
    fi
done
