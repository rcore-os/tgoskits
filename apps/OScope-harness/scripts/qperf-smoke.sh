#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
workspace="${STARRY_WORKSPACE:-}"
if [[ -z "$workspace" ]]; then
    workspace="$(git -C "$script_dir" rev-parse --show-toplevel 2>/dev/null || pwd)"
fi

if ! grep -q "pub case:" "$workspace/scripts/axbuild/src/starry/mod.rs" 2>/dev/null; then
    cat >&2 <<'EOF'
error: qperf smoke requires the Starry qperf runtime companion.

This app wrapper PR only packages OScope-harness/qperf entrypoints. Apply the
runtime companion or the combined runtime+app PR before running qperf smoke.
EOF
    exit 1
fi

checkout="$("$script_dir/../prebuild.sh")"

exec "$checkout/tools/starry-syscall-harness/scripts/qperf-smoke.sh" "$@"
