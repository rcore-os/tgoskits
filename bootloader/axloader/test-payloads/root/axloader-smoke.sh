#!/bin/sh
set -eu
test "$1" = x86-handoff
/bin/busybox mkdir -p /tmp
printf '%s' "$1" > /tmp/axloader-handoff
test "$(/bin/busybox cat /tmp/axloader-handoff)" = x86-handoff
printf '%s_AXLOADER_USERSPACE_PASSED\n' "$2"
# PID 1 must remain alive after publishing the result.
while :; do /bin/busybox sleep 1; done
