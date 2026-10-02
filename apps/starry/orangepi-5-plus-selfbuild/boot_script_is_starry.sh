#!/bin/sh
set -eu

[ "$#" = 1 ] || exit 2
boot_script=$1
[ -s "$boot_script" ] || exit 2

set +e
LC_ALL=C grep -aEiq 'starry|image[.]fit' "$boot_script"
grep_rc=$?
set -e
case "$grep_rc" in
    0|1) exit "$grep_rc" ;;
    *) exit 2 ;;
esac
