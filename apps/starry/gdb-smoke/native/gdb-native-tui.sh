#!/bin/sh
set -eu

if [ -z "${TERM:-}" ] || [ "${TERM:-}" = "dumb" ]; then
    export TERM=xterm
fi

if [ "${1:-}" = "--demo" ]; then
    shift
    exec gdb -q -tui -x /usr/bin/gdb-native-tui.gdb "${1:-/usr/bin/gdb-native-smoke-target}"
fi

exec gdb -q -tui "${1:-/usr/bin/gdb-native-smoke-target}"
