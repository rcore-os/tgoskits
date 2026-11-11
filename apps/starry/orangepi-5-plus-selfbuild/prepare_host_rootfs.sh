#!/bin/bash
set -euo pipefail

app_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$app_dir/../../.." && pwd)"
out_dir="$repo_root/target/starry-orangepi5plus-selfbuild/environment"
toolchain=$(sed -n 's/^[[:space:]]*channel[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$repo_root/rust-toolchain.toml")
case "$toolchain" in
    ''|*[!A-Za-z0-9._-]*) echo "invalid pinned Rust toolchain" >&2; exit 1 ;;
esac
command -v docker >/dev/null
mkdir -p "$out_dir"
recipe=$(sha256sum "$app_dir/rootfs/Dockerfile")
toolchain_recipe=$(sha256sum "$repo_root/rust-toolchain.toml")
cargo_recipe=$(sha256sum "$app_dir/rootfs/cargo-config.toml")
probe_dir="$app_dir/tests/host-link"
probe_manifest=$(sha256sum "$probe_dir/Cargo.toml")
probe_lock=$(sha256sum "$probe_dir/Cargo.lock")
probe_build=$(sha256sum "$probe_dir/build.rs")
probe_source=$(sha256sum "$probe_dir/src/lib.rs")
recipe="$toolchain:${recipe%% *}:${toolchain_recipe%% *}:${cargo_recipe%% *}:${probe_manifest%% *}:${probe_lock%% *}:${probe_build%% *}:${probe_source%% *}"
if [ -s "$out_dir/rootfs.tar" ] && [ -f "$out_dir/recipe-id" ] \
    && [ "$(cat "$out_dir/recipe-id")" = "$recipe" ]; then
    (cd "$out_dir" && sha256sum -c rootfs.sha256)
    echo "selfbuild_host_environment=reused"
    exit 0
fi
context="$out_dir/context"
cargo_home="${CARGO_HOME:-$HOME/.cargo}"
for kind in cache index; do
    mkdir -p "$context/registry/$kind"
    for directory in "$cargo_home/registry/$kind"/index.crates.io-*; do
        [ -d "$directory" ] || continue
        rsync -a "$directory" "$context/registry/$kind/"
    done
done
cp "$repo_root/rust-toolchain.toml" "$context/rust-toolchain.toml"
cp "$app_dir/rootfs/cargo-config.toml" "$context/cargo-config.toml"
mkdir -p "$context/host-link/src"
cp "$probe_dir/Cargo.toml" "$probe_dir/Cargo.lock" "$probe_dir/build.rs" "$context/host-link/"
cp "$probe_dir/src/lib.rs" "$context/host-link/src/"
image="tgoskits-selfbuild-arm64:$toolchain-musl"
build_options=()
# Host networking makes loopback proxies reachable by the build container.
if [ "$(uname -s)" = Linux ]; then
    build_options+=(--network host)
fi
for proxy in HTTP_PROXY HTTPS_PROXY NO_PROXY http_proxy https_proxy no_proxy; do
    if [ -n "${!proxy:-}" ]; then
        build_options+=(--build-arg "$proxy")
    fi
done
docker build "${build_options[@]}" --platform linux/arm64 --build-arg "RUST_TOOLCHAIN=$toolchain" \
    --file "$app_dir/rootfs/Dockerfile" --tag "$image" "$context"
image_id=$(docker image inspect --format '{{.Id}}' "$image")
if [ -s "$out_dir/rootfs.tar" ] && [ -f "$out_dir/image-id" ] \
    && [ "$(cat "$out_dir/image-id")" = "$image_id" ]; then
    (cd "$out_dir" && sha256sum -c rootfs.sha256)
    printf '%s\n' "$recipe" > "$out_dir/recipe-id"
    echo "selfbuild_host_environment=reused"
    exit 0
fi
container=$(docker create --platform linux/arm64 "$image")
cleanup() {
    docker rm "$container"
}
trap cleanup EXIT
docker export --output "$out_dir/rootfs.tar.new" "$container"
mv "$out_dir/rootfs.tar.new" "$out_dir/rootfs.tar"
(cd "$out_dir" && sha256sum rootfs.tar > rootfs.sha256)
printf '%s\n' "$image_id" > "$out_dir/image-id"
printf '%s\n' "$recipe" > "$out_dir/recipe-id"
echo "selfbuild_host_environment=$out_dir/rootfs.tar"
