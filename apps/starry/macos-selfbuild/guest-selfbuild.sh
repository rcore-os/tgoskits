#!/bin/sh
set -eu

marker="${MARKER:-STARRY-MACOS-SELFBUILD}"
jobs="${JOBS:-8}"
rayon_threads="${RAYON_NUM_THREADS:-1}"
rustc_threads="${RUSTC_THREADS:-2}"
cargo_bin="${CARGO_BIN:-/usr/bin/cargo}"
source_dir="${SOURCE_DIR:-/opt/tgoskits}"
work_dir="${WORK_DIR:-/tmp/starryos-selfbuild-src}"
target_dir="${CARGO_TARGET_DIR:-/tmp/starryos-selfbuild-target}"
source_tmpfs="${SOURCE_TMPFS:-1}"
source_tar="${SOURCE_TAR:-/opt/tgoskits-src.tar}"
source_meta="${SOURCE_META:-/opt/tgoskits-src.meta}"
profile="${PROFILE:-release}"
cargo_subcommand="${CARGO_SUBCOMMAND:-build}"
build_target="${BUILD_TARGET:-aarch64-unknown-none-softfloat}"
build_package="${BUILD_PACKAGE:-starryos}"
build_bin="${BUILD_BIN:-starryos}"
build_std="${BUILD_STD:-core,alloc,compiler_builtins}"
features="${FEATURES:-ax-feat/defplat,ax-feat/irq,ax-feat/ipi,ax-feat/rtc,cntv-timer,smp}"
no_default_features="${NO_DEFAULT_FEATURES:-0}"
allow_slow_selfbuild="${ALLOW_SLOW_SELFBUILD:-0}"

assert_fast_profile() {
    if [ "$allow_slow_selfbuild" = "1" ]; then
        echo "===${marker}-SLOW-PROFILE-ALLOWED==="
        return
    fi

    if [ "$rustc_threads" != "2" ]; then
        echo "===${marker}-FAST-PROFILE-ERROR rustc_threads=${rustc_threads} expected=2==="
        echo "Set RUSTC_THREADS=2 for the reproducible fast profile, or ALLOW_SLOW_SELFBUILD=1 for experiments."
        finish_guest 2
    fi

    case ",${features}," in
        *",plat-dyn,"*|*",ax-feat/display,"*|*",ax-driver/virtio-"*|*",starry-kernel/input,"*|*",starry-kernel/vsock,"*)
            echo "===${marker}-FAST-PROFILE-ERROR features=${features}==="
            echo "This feature set selects the slow full-device profile seen as about 386 crates."
            echo "Use the default feature-slim profile for reproduction, or set ALLOW_SLOW_SELFBUILD=1 for experiments."
            finish_guest 2
            ;;
    esac

    echo "===${marker}-FAST-PROFILE expected_crates~318==="
}

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

source_meta_path=""
for candidate in "$source_meta" "${source_dir}/.tgoskits-source-meta"; do
    if [ -f "$candidate" ]; then
        source_meta_path="$candidate"
        break
    fi
done

if [ -n "$source_meta_path" ]; then
    echo "===${marker}-SOURCE-META-BEGIN path=${source_meta_path}==="
    cat "$source_meta_path"
    echo "===${marker}-SOURCE-META-END==="
    actual_commit=""
    while IFS= read -r meta_line; do
        case "$meta_line" in
            commit=*) actual_commit="${meta_line#commit=}" ;;
        esac
    done < "$source_meta_path"
    if [ -n "${TGOSKITS_COMMIT:-}" ] && [ "$actual_commit" != "$TGOSKITS_COMMIT" ]; then
        echo "===${marker}-SOURCE-META-MISMATCH expected=${TGOSKITS_COMMIT} actual=${actual_commit}==="
        finish_guest 2
    fi
else
    echo "===${marker}-SOURCE-META-MISSING==="
fi

export PATH="/opt/rust-nightly/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
ld_library_path="/opt/rust-nightly/lib:/usr/lib"
for candidate in /usr/lib/llvm*/lib; do
    if [ -d "$candidate" ]; then
        ld_library_path="${ld_library_path}:${candidate}"
    fi
done
export LD_LIBRARY_PATH="${ld_library_path}:${LD_LIBRARY_PATH:-}"
export RUSTC="${RUSTC:-/opt/rustc-nightly-sysroot}"
export RUSTDOC="${RUSTDOC:-/opt/rustdoc-nightly-sysroot}"
export HOME="${HOME:-/root}"
export CARGO_HOME="${CARGO_HOME:-/root/.cargo}"
export RUSTC_BOOTSTRAP="${RUSTC_BOOTSTRAP:-1}"
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"
export CARGO_NET_OFFLINE="${CARGO_NET_OFFLINE:-true}"
export CARGO_BUILD_JOBS="$jobs"
export RAYON_NUM_THREADS="$rayon_threads"
export CARGO_TARGET_DIR="$target_dir"

if [ -z "${LIBCLANG_PATH:-}" ]; then
    for candidate in /usr/lib /usr/lib/llvm*/lib; do
        if [ -e "${candidate}/libclang.so" ]; then
            export LIBCLANG_PATH="$candidate"
            break
        fi
    done
fi

export CARGO_PROFILE_RELEASE_LTO="${CARGO_PROFILE_RELEASE_LTO:-false}"
export CARGO_PROFILE_RELEASE_OPT_LEVEL="${CARGO_PROFILE_RELEASE_OPT_LEVEL:-0}"
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS="${CARGO_PROFILE_RELEASE_CODEGEN_UNITS:-256}"
export CARGO_PROFILE_RELEASE_DEBUG="${CARGO_PROFILE_RELEASE_DEBUG:-0}"

if [ "$source_tmpfs" = "1" ]; then
    echo "===${marker}-SOURCE-COPY-BEGIN from=${source_dir} to=${work_dir}==="
    rm -rf "$work_dir"
    mkdir -p "$work_dir"
    for path in Cargo.toml Cargo.lock rust-toolchain.toml .tgoskits-source-meta .cargo apps components drivers memory os platforms scripts test-suit tools vendor virtualization xtask; do
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

if [ -d "apps/starry/macos-selfbuild/crates/lwprintf-rs" ] \
    && ! grep -q "apps/starry/macos-selfbuild/crates/lwprintf-rs" Cargo.toml; then
    cat >>Cargo.toml <<'PATCH_CARGO'

[patch.crates-io]
lwprintf-rs = { path = "apps/starry/macos-selfbuild/crates/lwprintf-rs" }
PATCH_CARGO
fi

if [ -n "${AX_CONFIG_PATH:-}" ]; then
    export AX_CONFIG_PATH
elif [ -f "$(pwd)/os/StarryOS/.axconfig.toml" ]; then
    export AX_CONFIG_PATH="$(pwd)/os/StarryOS/.axconfig.toml"
else
    unset AX_CONFIG_PATH
fi
rustflags="${EXTRA_RUSTFLAGS:-}"
if [ -n "$rustc_threads" ]; then
    rustflags="${rustflags} -Zthreads=${rustc_threads}"
fi
export RUSTFLAGS="${rustflags} -Clink-arg=-Tlinker.x -Clink-arg=-no-pie -Clink-arg=-znostart-stop-gc"

echo "===${marker}-ENV-BEGIN==="
echo "jobs=${jobs}"
echo "rayon_num_threads=${RAYON_NUM_THREADS}"
echo "rustc_threads=${rustc_threads}"
echo "profile=${profile}"
echo "cargo_subcommand=${cargo_subcommand}"
echo "build_package=${build_package}"
echo "build_bin=${build_bin}"
echo "build_target=${build_target}"
echo "build_std=${build_std}"
echo "features=${features}"
echo "allow_slow_selfbuild=${allow_slow_selfbuild}"
echo "source_dir=${source_dir}"
echo "target_dir=${target_dir}"
echo "work_dir=$(pwd)"
echo "cargo_home=${CARGO_HOME}"
echo "ax_config_path=${AX_CONFIG_PATH:-}"
echo "libclang_path=${LIBCLANG_PATH:-}"
echo "rustflags=${RUSTFLAGS}"
"$RUSTC" --version || true
"$cargo_bin" --version || true
echo "===${marker}-ENV-END==="

assert_fast_profile

set -- "$cargo_bin" "$cargo_subcommand" \
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
