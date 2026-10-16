#!/bin/bash
set -euo pipefail

app_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
repo_root="$(cd "$app_dir/../../.." && pwd)"
mkdir -p "$repo_root/tmp"
test_dir="$(mktemp -d -p "$repo_root/tmp" guest-clock.XXXXXX)"
cleanup() {
    rm -rf "$test_dir"
}
trap cleanup EXIT

mkdir -p "$test_dir/bin"
printf '#!/bin/sh\nexit 1\n' > "$test_dir/bin/date"
chmod +x "$test_dir/bin/date"

clock_output="$(
    PATH="$test_dir/bin" \
        "$app_dir/set_guest_clock.sh" TEST-SELFBUILD 1787940000
)"
[ "$clock_output" = \
    '===TEST-SELFBUILD-CLOCK-UNAVAILABLE requested_epoch=1787940000===' ] || {
    echo "unexpected unavailable-clock output: $clock_output" >&2
    exit 1
}

echo "orangepi5plus_guest_clock_fallback=PASS"
