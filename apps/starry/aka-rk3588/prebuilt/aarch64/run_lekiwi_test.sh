#!/bin/sh
# Safe raised-frame test alias.

set -eu

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
exec "${SCRIPT_DIR}/run_lekiwi_loop.sh" "$@"
