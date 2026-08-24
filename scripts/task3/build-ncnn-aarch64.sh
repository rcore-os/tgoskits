#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ncnn_source="${NCNN_SOURCE:-}"
out_dir="${OUT_DIR:-$repo_root/tmp/task3-yolo/ncnn-aarch64}"
cross_root="${CROSS_ROOT:-/home/huhu/.local/toolchains/aarch64-linux-musl-cross}"
expected_revision="946fe3fb14a8dff8c06df763f67be522167b2f00"

if [[ -z "$ncnn_source" || ! -f "$ncnn_source/CMakeLists.txt" ]]; then
    printf 'error: NCNN_SOURCE must point to the pinned ncnn checkout\n' >&2
    exit 2
fi
actual_revision="$(git -C "$ncnn_source" rev-parse HEAD 2>/dev/null || true)"
if [[ "$actual_revision" != "$expected_revision" ]]; then
    printf 'error: ncnn revision mismatch: expected %s got %s\n' \
        "$expected_revision" "${actual_revision:-unknown}" >&2
    exit 2
fi
for tool in gcc g++ ar ranlib; do
    if [[ ! -x "$cross_root/bin/aarch64-linux-musl-$tool" ]]; then
        printf 'error: missing cross tool %s under %s\n' "$tool" "$cross_root/bin" >&2
        exit 2
    fi
done

mkdir -p "$out_dir"
toolchain="$out_dir/toolchain.cmake"
build_dir="$out_dir/build"
install_dir="$out_dir/install"
cat > "$toolchain" <<EOF
set(CMAKE_SYSTEM_NAME Linux)
set(CMAKE_SYSTEM_PROCESSOR aarch64)
set(CMAKE_C_COMPILER "$cross_root/bin/aarch64-linux-musl-gcc")
set(CMAKE_CXX_COMPILER "$cross_root/bin/aarch64-linux-musl-g++")
set(CMAKE_AR "$cross_root/bin/aarch64-linux-musl-ar")
set(CMAKE_RANLIB "$cross_root/bin/aarch64-linux-musl-ranlib")
set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)
set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_PACKAGE ONLY)
EOF

cmake -S "$ncnn_source" -B "$build_dir" -G Ninja \
    -DCMAKE_TOOLCHAIN_FILE="$toolchain" \
    -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_INSTALL_PREFIX="$install_dir" \
    -DNCNN_BUILD_SHARED_LIBS=OFF \
    -DNCNN_BUILD_TOOLS=OFF \
    -DNCNN_BUILD_EXAMPLES=OFF \
    -DNCNN_BUILD_TESTS=OFF \
    -DNCNN_BUILD_BENCHMARK=OFF \
    -DNCNN_VULKAN=OFF \
    -DNCNN_OPENMP=OFF \
    -DNCNN_RUNTIME_CPU=ON \
    -DNCNN_BUILD_ANDROID_PROJECT=OFF
cmake --build "$build_dir" --target ncnn --parallel "${JOBS:-2}"
cmake --install "$build_dir"

{
    printf 'ncnn_git_revision = "%s"\n' "$actual_revision"
    printf 'target = "aarch64-linux-musl"\n'
    printf 'shared = false\n'
    printf 'vulkan = false\n'
    printf 'openmp = false\n'
    printf 'runtime_cpu = true\n'
} > "$out_dir/manifest.toml"
printf 'ncnn_install=%s\nmanifest=%s\n' "$install_dir" "$out_dir/manifest.toml"
