#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../../../.." && pwd)"
case_dir="$repo_root/test-suit/axvisor/normal/board-orangepi-5-plus/ivc-bench"
out_dir="$repo_root/tmp/axbuild/axvisor/ivc-bench"
target_dir="$repo_root/target/aarch64-unknown-linux-musl/release"
objcopy="$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/^host: //p')/bin/llvm-objcopy"

mkdir -p "$out_dir"

build_guest() {
  local package="$1"
  local output="$2"
  local config="$3"

  cargo xtask arceos build -p "$package" -c "$config"
  "$objcopy" -O binary "$target_dir/$package" "$out_dir/$output"
  ls -lh "$out_dir/$output"
}

cd "$repo_root"
build_guest arceos-ivc-bench-publisher arceos-ivc-bench-publisher.bin "$case_dir/arceos-build/publisher.toml"
build_guest arceos-ivc-bench-subscriber arceos-ivc-bench-subscriber.bin "$case_dir/arceos-build/subscriber.toml"
