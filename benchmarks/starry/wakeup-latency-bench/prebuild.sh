#!/usr/bin/env bash
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
overlay_dir="${STARRY_OVERLAY_DIR:-}"

if [[ -z "$overlay_dir" ]]; then
    echo "error: STARRY_OVERLAY_DIR is required" >&2
    exit 1
fi
if [[ "${STARRY_ARCH:-}" != "x86_64" ]]; then
    echo "error: wakeup-latency-bench currently supports x86_64 only" >&2
    exit 1
fi

compilers=(x86_64-linux-musl-gcc musl-gcc x86_64-linux-gnu-gcc gcc)
cc=""
for candidate in "${compilers[@]}"; do
    if command -v "$candidate" >/dev/null 2>&1; then
        cc="$candidate"
        break
    fi
done
if [[ -z "$cc" ]]; then
    echo "error: no static x86_64 C compiler found" >&2
    exit 1
fi

build_dir="$(mktemp -d)"
trap 'rm -rf "$build_dir"' EXIT

"$cc" \
    -std=c11 -O2 -Wall -Wextra -Werror -pthread -static \
    -I"$app_dir" \
    "$app_dir/tests/handoff-spurious.c" "$app_dir/stats.c" \
    -Wl,--wrap=syscall -lm -o "$build_dir/handoff-spurious-test"
"$build_dir/handoff-spurious-test"

"$cc" \
    -std=c11 \
    -O2 \
    -Wall \
    -Wextra \
    -Werror \
    -pthread \
    -static \
    "$app_dir/main.c" \
    "$app_dir/baseline.c" \
    "$app_dir/handoff.c" \
    "$app_dir/timer.c" \
    "$app_dir/yield.c" \
    "$app_dir/stats.c" \
    -lm \
    -o "$build_dir/wakeup-latency-bench"

install -Dm0755 \
    "$build_dir/wakeup-latency-bench" \
    "$overlay_dir/usr/bin/wakeup-latency-bench"
install -Dm0755 \
    "$app_dir/wakeup-latency-bench.sh" \
    "$overlay_dir/usr/bin/wakeup-latency-bench.sh"

# The minimal Alpine image used by this benchmark does not provide an init
# candidate that StarryOS can execute.  Supply a tiny PID 1 so the shell check
# can reach the installed benchmark runner.
cat >"$overlay_dir/init" <<'EOF'
#!/bin/sh
exec /bin/sh
EOF
chmod 0755 "$overlay_dir/init"
