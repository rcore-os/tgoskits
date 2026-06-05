#!/usr/bin/env bash
set -euo pipefail

workspace="${STARRY_WORKSPACE:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)}"
asset_dir="$workspace/target/deepseek/assets"
src_dir="$workspace/target/deepseek/build/deepseek-tui"
repo="https://github.com/Hmbown/DeepSeek-TUI.git"
tag="v0.8.18"

need_cmd() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "error: required command not found: $1" >&2
        exit 1
    fi
}

need_cmd install

rust_musl_target="x86_64-unknown-linux-musl"

ensure_rust_musl_target() {
    need_cmd rustup

    local toolchain
    toolchain="$(rustup show active-toolchain)"
    toolchain="${toolchain%% *}"
    if rustup target list --installed --toolchain "$toolchain" | grep -qx "$rust_musl_target"; then
        return
    fi

    echo "Installing Rust target $rust_musl_target for toolchain $toolchain..."
    rustup target add --toolchain "$toolchain" "$rust_musl_target"
}

if [ -x "$asset_dir/deepseek" ] && [ -x "$asset_dir/deepseek-tui" ]; then
    echo "DeepSeek TUI assets already staged in $asset_dir"
    file "$asset_dir/deepseek" 2>/dev/null | head -1
    ls -lh "$asset_dir/deepseek"
    exit 0
fi

mkdir -p "$asset_dir" "$src_dir"

docker_build() {
    local script_dir
    script_dir="$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")"
    echo "Building deepseek $tag in Docker (Alpine + musl)..."
    docker build --network host \
        -f "$script_dir/Dockerfile.build" \
        -t deepseek-tui-builder \
        "$workspace" 2>&1
    echo ""
    echo "Extracting binaries and shared libraries..."
    local container
    container="$(docker create --network none deepseek-tui-builder)"
    trap 'docker rm -f "$container" >/dev/null 2>&1 || true' RETURN

    docker cp "$container:/deepseek" "$asset_dir/deepseek"
    docker cp "$container:/deepseek-tui" "$asset_dir/deepseek-tui"
    chmod 0755 "$asset_dir/deepseek" "$asset_dir/deepseek-tui"

    mkdir -p "$asset_dir/lib"
    docker cp "$container:/usr/lib/." "$asset_dir/lib/" 2>/dev/null || true
    docker rm -f "$container" >/dev/null
    trap - RETURN

    echo ""
    file "$asset_dir/deepseek" 2>/dev/null || true
    LD_LIBRARY_PATH="$asset_dir/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" "$asset_dir/deepseek" --version 2>/dev/null || echo "(version check skipped — musl binary needs musl host)"
    echo ""
    echo "DeepSeek TUI assets ready in $asset_dir"
    ls -lh "$asset_dir/"
    exit 0
}

direct_build() {
    need_cmd git
    need_cmd cargo
    need_cmd strip

    echo "Cloning DeepSeek TUI $tag..."
    if [ -d "$src_dir/.git" ]; then
        echo "  Updating existing clone..."
        cd "$src_dir" && git fetch --tags --depth 1 origin "$tag" && git checkout -f "$tag"
    else
        git clone --depth 1 --branch "$tag" "$repo" "$src_dir"
    fi

    echo ""
    echo "Building deepseek ($tag)..."
    cd "$src_dir"
    ensure_rust_musl_target

    echo "  Building deepseek (crates/cli) with musl target..."
    cargo build --release --locked --target x86_64-unknown-linux-musl --bin deepseek

    echo "  Building deepseek-tui (crates/tui) with musl target..."
    cargo build --release --locked --target x86_64-unknown-linux-musl --bin deepseek-tui

    echo ""
    echo "Stripping and staging binaries..."
    install -m 0755 "$src_dir/target/x86_64-unknown-linux-musl/release/deepseek" "$asset_dir/deepseek"
    install -m 0755 "$src_dir/target/x86_64-unknown-linux-musl/release/deepseek-tui" "$asset_dir/deepseek-tui"
    strip "$asset_dir/deepseek" "$asset_dir/deepseek-tui"

    echo ""
    file "$asset_dir/deepseek"
    "$asset_dir/deepseek" --version
    "$asset_dir/deepseek-tui" --version

    echo ""
    echo "DeepSeek TUI assets ready in $asset_dir"
    ls -lh "$asset_dir/"
}

if command -v docker >/dev/null 2>&1; then
    docker_build
else
    direct_build
fi
