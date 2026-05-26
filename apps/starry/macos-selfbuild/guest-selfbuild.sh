#!/bin/sh
set -eu

marker="${MARKER:-STARRY-MACOS-SELFBUILD}"
jobs="${JOBS:-8}"
source_dir="${SOURCE_DIR:-/opt/tgoskits}"
work_dir="${WORK_DIR:-/tmp/starryos-selfbuild-src}"
target_dir="${CARGO_TARGET_DIR:-/tmp/starryos-selfbuild-target}"
source_tmpfs="${SOURCE_TMPFS:-1}"
source_tar="${SOURCE_TAR:-/opt/tgoskits-src.tar}"
profile="${PROFILE:-release}"
cargo_subcommand="${CARGO_SUBCOMMAND:-build}"
build_target="${BUILD_TARGET:-aarch64-unknown-none-softfloat}"
build_package="${BUILD_PACKAGE:-starryos}"
build_bin="${BUILD_BIN:-starryos}"
build_std="${BUILD_STD:-core,alloc,compiler_builtins}"
features="${FEATURES:-qemu,gic-v3,cntv-timer,smp}"
no_default_features="${NO_DEFAULT_FEATURES:-0}"

finish_guest() {
    rc="$1"
    echo "===${marker}-RUN-END rc=${rc}==="
    sync 2>/dev/null || true
    if command -v poweroff >/dev/null 2>&1; then
        poweroff -f 2>/dev/null || poweroff 2>/dev/null || true
    elif command -v halt >/dev/null 2>&1; then
        halt -f 2>/dev/null || halt 2>/dev/null || true
    fi
    exit "$rc"
}

echo "===${marker}-BEGIN jobs=${jobs} source_tmpfs=${source_tmpfs}==="
echo "guest_cpu_count=$(grep -c '^processor' /proc/cpuinfo 2>/dev/null || true)"

if [ ! -f "${source_dir}/Cargo.toml" ] && [ -f "$source_tar" ]; then
    echo "===${marker}-SOURCE-TAR-EXTRACT-BEGIN tar=${source_tar}==="
    tar_work="/tmp/tgoskits-src-from-tar"
    rm -rf "$tar_work"
    mkdir -p "$tar_work"
    tar -xf "$source_tar" -C "$tar_work"
    source_dir="$tar_work"
    echo "===${marker}-SOURCE-TAR-EXTRACT-END dir=${source_dir}==="
fi

export PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/opt/rust-nightly/bin"
export LD_LIBRARY_PATH="/usr/lib:/opt/rust-nightly/lib:${LD_LIBRARY_PATH:-}"
export RUSTC="${RUSTC:-/opt/rustc-nightly-sysroot}"
export RUSTDOC="${RUSTDOC:-/opt/rustdoc-nightly-sysroot}"
export RUSTC_BOOTSTRAP="${RUSTC_BOOTSTRAP:-1}"
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"
export CARGO_NET_OFFLINE="${CARGO_NET_OFFLINE:-true}"
export CARGO_BUILD_JOBS="$jobs"
export RAYON_NUM_THREADS="$jobs"
export CARGO_TARGET_DIR="$target_dir"

export CARGO_PROFILE_RELEASE_LTO="${CARGO_PROFILE_RELEASE_LTO:-false}"
export CARGO_PROFILE_RELEASE_OPT_LEVEL="${CARGO_PROFILE_RELEASE_OPT_LEVEL:-0}"
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS="${CARGO_PROFILE_RELEASE_CODEGEN_UNITS:-256}"
export CARGO_PROFILE_RELEASE_DEBUG="${CARGO_PROFILE_RELEASE_DEBUG:-0}"

if [ "$source_tmpfs" = "1" ]; then
    echo "===${marker}-SOURCE-COPY-BEGIN from=${source_dir} to=${work_dir}==="
    rm -rf "$work_dir"
    mkdir -p "$work_dir"
    for path in Cargo.toml Cargo.lock rust-toolchain.toml .cargo apps components drivers os platform scripts test-suit tools vendor xtask; do
        if [ -e "${source_dir}/${path}" ]; then
            cp -a "${source_dir}/${path}" "${work_dir}/"
        fi
    done
    if [ -f "${work_dir}/.cargo/config.toml" ]; then
        sed -i "s#${source_dir}/vendor#${work_dir}/vendor#g" "${work_dir}/.cargo/config.toml" || true
    fi
    echo "===${marker}-SOURCE-COPY-END==="
    cd "$work_dir"
else
    cd "$source_dir"
fi

export AX_CONFIG_PATH="${AX_CONFIG_PATH:-$(pwd)/os/StarryOS/.axconfig.toml}"
export RUSTFLAGS="${EXTRA_RUSTFLAGS:-} -Clink-arg=-Tlinker.x -Clink-arg=-no-pie -Clink-arg=-znostart-stop-gc"

echo "===${marker}-ENV-BEGIN==="
echo "jobs=${jobs}"
echo "profile=${profile}"
echo "cargo_subcommand=${cargo_subcommand}"
echo "build_package=${build_package}"
echo "build_bin=${build_bin}"
echo "build_target=${build_target}"
echo "build_std=${build_std}"
echo "features=${features}"
echo "source_dir=${source_dir}"
echo "target_dir=${target_dir}"
echo "work_dir=$(pwd)"
echo "rustflags=${RUSTFLAGS}"
"$RUSTC" --version || true
/usr/bin/cargo --version || true
echo "===${marker}-ENV-END==="

set -- /usr/bin/cargo "$cargo_subcommand" \
    -p "$build_package" \
    --bin "$build_bin" \
    --target "$build_target"

if [ -n "$build_std" ] && [ "$build_std" != "none" ]; then
    set -- "$@" -Z "build-std=${build_std}"
fi

set -- "$@" --target-dir "$target_dir"

if [ "$no_default_features" = "1" ]; then
    set -- "$@" --no-default-features
fi

if [ -n "$features" ]; then
    set -- "$@" --features "$features"
fi

if [ "$profile" = "release" ]; then
    set -- "$@" --release
fi

echo "===${marker}-CARGO-COMMAND==="
printf '%s\n' "$*"
start="$(date +%s)"
echo "===${marker}-START jobs=${jobs} start=${start}==="

set +e
"$@"
rc="$?"
set -e

end="$(date +%s)"
elapsed="$((end - start))"
echo "===${marker}-END jobs=${jobs} rc=${rc} elapsed=${elapsed}==="

if [ "$profile" = "release" ]; then
    artifact="${target_dir}/${build_target}/release/${build_bin}"
else
    artifact="${target_dir}/${build_target}/debug/${build_bin}"
fi

if [ "$rc" = "0" ]; then
    if [ -f "$artifact" ]; then
        bytes="$(wc -c <"$artifact" 2>/dev/null || echo unknown)"
        echo "===${marker}-ARTIFACT path=${artifact} bytes=${bytes}==="
    fi
    echo "===${marker}-PASS jobs=${jobs} elapsed=${elapsed}==="
else
    echo "===${marker}-FAIL jobs=${jobs} rc=${rc} elapsed=${elapsed}==="
fi

finish_guest "$rc"
