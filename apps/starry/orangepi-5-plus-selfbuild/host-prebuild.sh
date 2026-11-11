#!/bin/bash
set -euo pipefail

app_dir="${STARRY_CASE_DIR:?}"
[ "${STARRY_ARCH:?}" = aarch64 ] || { echo "selfbuild requires aarch64" >&2; exit 1; }
bash "$app_dir/prepare_host_rootfs.sh"
repo_root="$(cd "$app_dir/../../.." && pwd)"
cd "$repo_root"
cargo fetch --locked --offline --target aarch64-unknown-linux-musl \
    || cargo fetch --locked --target aarch64-unknown-linux-musl
cargo fetch --locked --offline --target aarch64-unknown-none-softfloat -Z build-std=core,alloc \
    || cargo fetch --locked --target aarch64-unknown-none-softfloat -Z build-std=core,alloc
python3 "$app_dir/prepare_image.py"
