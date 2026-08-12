#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
cc="${CROSS_CC:-$HOME/.local/toolchains/aarch64-linux-musl-cross/bin/aarch64-linux-musl-gcc}"
out_dir="${OUT_DIR:-$repo_root/tmp/net-dual-guest}"
mkdir -p "$out_dir"
"$cc" -static -no-pie -O2 -Wall -Wextra -Werror -s \
  -o "$out_dir/udp_probe" "$repo_root/scripts/test/net-dual-guest/udp_probe.c"
"$cc" -static -no-pie -O2 -Wall -Wextra -Werror -s \
  -o "$out_dir/task2-init" "$repo_root/scripts/test/net-dual-guest/task2-init.c"
printf 'built %s\n' "$out_dir/udp_probe"
printf 'built %s\n' "$out_dir/task2-init"
