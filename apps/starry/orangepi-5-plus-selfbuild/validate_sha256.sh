#!/bin/bash
set -euo pipefail

sha256=${1:-}
[ "${#sha256}" = 64 ] || exit 1
case "$sha256" in
    *[!0-9a-f]*) exit 1 ;;
esac
