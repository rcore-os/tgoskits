#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/env.sh
source "$ROOT/scripts/env.sh"

if [[ ! -x "$AKARS_TENNIS_CC" ]]; then
  echo "error: linker not found: $AKARS_TENNIS_CC" >&2
  echo "       run scripts/setup.sh or set AKARS_TENNIS_TOOLCHAIN_DIR" >&2
  exit 1
fi

exec "$AKARS_TENNIS_CC" "$@"
