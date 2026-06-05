#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
workspace="${STARRY_WORKSPACE:-}"
if [[ -z "$workspace" ]]; then
    workspace="$(git -C "$script_dir" rev-parse --show-toplevel 2>/dev/null || pwd)"
fi

if ! grep -q "pub case:" "$workspace/scripts/axbuild/src/starry/mod.rs" 2>/dev/null; then
    cat >&2 <<'EOF'
error: qperf smoke requires the enhanced Starry qperf runtime.

The current checkout does not expose the `cargo xtask starry perf --case`
runtime interface needed by the OScope-harness qperf smoke wrapper.
EOF
    exit 1
fi

checkout="$("$script_dir/../prebuild.sh")"

exec "$checkout/tools/starry-syscall-harness/scripts/qperf-smoke.sh" "$@"
