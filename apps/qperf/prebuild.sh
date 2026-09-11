#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
workspace="$(cd "$script_dir/../.." && pwd)"

if [[ $# -gt 0 ]]; then
    cd "$workspace"
    exec "$@"
fi

printf '%s\n' "$workspace"
