#!/bin/sh
set -eu

marker="${MARKER:-STARRY-MACOS-SELFBUILD}"
jobs="${JOBS:-4}"
rayon_threads="${RAYON_NUM_THREADS:-1}"
rustc_threads="${RUSTC_THREADS:-2}"
cargo_bin="${CARGO_BIN:-/opt/cargo-nightly-sysroot}"
source_dir="${SOURCE_DIR:-/opt/tgoskits}"
work_dir="${WORK_DIR:-/tmp/starryos-selfbuild-src}"
target_dir="${CARGO_TARGET_DIR:-/tmp/starryos-selfbuild-target}"
artifact_dir="${ARTIFACT_DIR:-/opt/starryos-selfbuild-artifacts}"
source_tmpfs="${SOURCE_TMPFS:-1}"
source_tar="${SOURCE_TAR:-/opt/tgoskits-src.tar}"
source_meta="${SOURCE_META:-/opt/tgoskits-src.meta}"
profile="release"
build_target="aarch64-unknown-none-softfloat"
build_package="starryos"
build_bin="starryos"
build_std="core,alloc"
build_std_features="compiler-builtins-mem"
features="${FEATURES:-plat-dyn,ax-driver/virtio-blk,ax-driver/virtio-net,smp}"
cargo_verbose="${CARGO_VERBOSE:-0}"
artifact_to_bin="${ARTIFACT_TO_BIN:-1}"
kallsyms_reserved="${STARRY_KALLSYMS_RESERVED:-16M}"

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

find_first_executable() {
    for candidate do
        if [ -x "$candidate" ]; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done
    return 1
}

ensure_tool_wrapper() {
    wrapper_dir="$1"
    rust_tool="$2"
    llvm_tool="$3"

    if command -v "$rust_tool" >/dev/null 2>&1 && "$rust_tool" --version >/dev/null 2>&1; then
        return
    fi

    llvm_path="$(find_first_executable \
        "/usr/bin/${llvm_tool}" \
        "/usr/lib/llvm22/bin/${llvm_tool}" \
        "/usr/lib/llvm21/bin/${llvm_tool}" \
        "/usr/lib/llvm20/bin/${llvm_tool}" \
        "/usr/lib/llvm/bin/${llvm_tool}" || true)"
    if [ -z "$llvm_path" ]; then
        echo "===${marker}-KALLSYMS-TOOL-MISSING tool=${rust_tool} fallback=${llvm_tool}==="
        finish_guest 2
    fi

    cat >"${wrapper_dir}/${rust_tool}" <<EOF
#!/bin/sh
exec "${llvm_path}" "\$@"
EOF
    chmod +x "${wrapper_dir}/${rust_tool}"
}

ensure_gen_ksym() {
    install_root="$1"

    if command -v gen_ksym >/dev/null 2>&1; then
        return
    fi

    ksym_manifest=""
    for candidate in /root/.cargo/registry/src/*/ksym-0.6.0/Cargo.toml; do
        if [ -f "$candidate" ]; then
            ksym_manifest="$candidate"
            break
        fi
    done
    if [ -z "$ksym_manifest" ]; then
        echo "===${marker}-KALLSYMS-TOOL-MISSING tool=gen_ksym crate=ksym-0.6.0==="
        finish_guest 2
    fi

    echo "===${marker}-KALLSYMS-GEN-KSYM-BUILD manifest=${ksym_manifest}==="
    RUSTFLAGS= CARGO_ENCODED_RUSTFLAGS= "$cargo_bin" install \
        --offline \
        --locked \
        --path "$(dirname "$ksym_manifest")" \
        --root "$install_root" \
        --bin gen_ksym
}

ensure_kallsyms_tools() {
    tools_root="/tmp/starryos-selfbuild-tools"
    wrapper_dir="${tools_root}/wrappers"
    install_root="${tools_root}/cargo-install"
    mkdir -p "$wrapper_dir" "$install_root"

    export PATH="${install_root}/bin:${wrapper_dir}:${PATH}"
    ensure_tool_wrapper "$wrapper_dir" rust-nm llvm-nm
    ensure_tool_wrapper "$wrapper_dir" rust-objdump llvm-objdump
    ensure_tool_wrapper "$wrapper_dir" rust-objcopy llvm-objcopy
    ensure_gen_ksym "$install_root"

    echo "===${marker}-KALLSYMS-TOOLS-READY==="
}

run_starry_kallsyms() {
    artifact="$1"

    kallsyms_script="apps/starry/macos-selfbuild/starry-kallsyms.sh"
    if [ ! -f "$kallsyms_script" ]; then
        echo "===${marker}-KALLSYMS-SCRIPT-MISSING==="
        finish_guest 2
    fi

    ensure_kallsyms_tools
    echo "===${marker}-KALLSYMS-BEGIN elf=${artifact}==="
    set +e
    KERNEL_ELF="$artifact" AXBUILD_STARRY_KALLSYMS_AUTO_INSTALL=0 \
        sh "$kallsyms_script"
    kallsyms_rc="$?"
    set -e
    if [ "$kallsyms_rc" != "0" ]; then
        echo "===${marker}-KALLSYMS-FAIL rc=${kallsyms_rc}==="
        finish_guest "$kallsyms_rc"
    fi
    echo "===${marker}-KALLSYMS-END elf=${artifact}==="
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
export AX_ARCH="${AX_ARCH:-aarch64}"
export AX_TARGET="${AX_TARGET:-$build_target}"
export AX_LOG="${AX_LOG:-warn}"
export STARRY_KALLSYMS_RESERVED="$kallsyms_reserved"

if [ -z "${LIBCLANG_PATH:-}" ]; then
    for candidate in /usr/lib /usr/lib/llvm*/lib; do
        if [ -e "${candidate}/libclang.so" ]; then
            export LIBCLANG_PATH="$candidate"
            break
        fi
    done
fi

export CC_aarch64_unknown_none_softfloat="${CC_aarch64_unknown_none_softfloat:-aarch64-linux-musl-gcc}"
export AR_aarch64_unknown_none_softfloat="${AR_aarch64_unknown_none_softfloat:-aarch64-linux-musl-ar}"
export CFLAGS_aarch64_unknown_none_softfloat="${CFLAGS_aarch64_unknown_none_softfloat:--mgeneral-regs-only -ffreestanding -fno-builtin -fPIC}"
export CC_AARCH64_UNKNOWN_NONE_SOFTFLOAT="${CC_AARCH64_UNKNOWN_NONE_SOFTFLOAT:-$CC_aarch64_unknown_none_softfloat}"
export AR_AARCH64_UNKNOWN_NONE_SOFTFLOAT="${AR_AARCH64_UNKNOWN_NONE_SOFTFLOAT:-$AR_aarch64_unknown_none_softfloat}"
export CFLAGS_AARCH64_UNKNOWN_NONE_SOFTFLOAT="${CFLAGS_AARCH64_UNKNOWN_NONE_SOFTFLOAT:-$CFLAGS_aarch64_unknown_none_softfloat}"
export CC_target_aarch64_unknown_none_softfloat_pie="${CC_target_aarch64_unknown_none_softfloat_pie:-aarch64-linux-musl-gcc}"
export AR_target_aarch64_unknown_none_softfloat_pie="${AR_target_aarch64_unknown_none_softfloat_pie:-aarch64-linux-musl-ar}"
export CFLAGS_target_aarch64_unknown_none_softfloat_pie="${CFLAGS_target_aarch64_unknown_none_softfloat_pie:--mgeneral-regs-only -ffreestanding -fno-builtin -fPIC}"
export CC_TARGET_AARCH64_UNKNOWN_NONE_SOFTFLOAT_PIE="${CC_TARGET_AARCH64_UNKNOWN_NONE_SOFTFLOAT_PIE:-$CC_target_aarch64_unknown_none_softfloat_pie}"
export AR_TARGET_AARCH64_UNKNOWN_NONE_SOFTFLOAT_PIE="${AR_TARGET_AARCH64_UNKNOWN_NONE_SOFTFLOAT_PIE:-$AR_target_aarch64_unknown_none_softfloat_pie}"
export CFLAGS_TARGET_AARCH64_UNKNOWN_NONE_SOFTFLOAT_PIE="${CFLAGS_TARGET_AARCH64_UNKNOWN_NONE_SOFTFLOAT_PIE:-$CFLAGS_target_aarch64_unknown_none_softfloat_pie}"

if [ -n "${CARGO_PROFILE_RELEASE_LTO+x}" ]; then export CARGO_PROFILE_RELEASE_LTO; fi
if [ -n "${CARGO_PROFILE_RELEASE_OPT_LEVEL+x}" ]; then export CARGO_PROFILE_RELEASE_OPT_LEVEL; fi
if [ -n "${CARGO_PROFILE_RELEASE_CODEGEN_UNITS+x}" ]; then export CARGO_PROFILE_RELEASE_CODEGEN_UNITS; fi
if [ -n "${CARGO_PROFILE_RELEASE_DEBUG+x}" ]; then export CARGO_PROFILE_RELEASE_DEBUG; fi

sanitize_cargo_config() {
    config_dir="$1"
    if [ -f "${config_dir}/.cargo/config.toml" ]; then
        sed -i "s#${source_dir}/vendor#${config_dir}/vendor#g" "${config_dir}/.cargo/config.toml" || true
        sed -i '/^include[[:space:]]*=/d' "${config_dir}/.cargo/config.toml" || true
    fi
}

configure_host_rustflags() {
    config="${CARGO_HOME}/config.toml"
    mkdir -p "${CARGO_HOME}"
    touch "$config"
    if ! grep -q 'starry-macos-selfbuild-host-rustflags' "$config"; then
        cat >>"$config" <<'EOF'

# starry-macos-selfbuild-host-rustflags
[host]
rustflags = ["-C", "target-feature=-crt-static"]
EOF
    fi
}

if [ "$source_tmpfs" = "1" ]; then
    echo "===${marker}-SOURCE-COPY-BEGIN from=${source_dir} to=${work_dir}==="
    rm -rf "$work_dir"
    mkdir -p "$work_dir"
    for path in Cargo.toml Cargo.lock rust-toolchain.toml .tgoskits-source-meta .cargo apps bootloader components drivers memory net os platforms scripts test-suit tools vendor virtualization xtask; do
        if [ -e "${source_dir}/${path}" ]; then
            cp -a "${source_dir}/${path}" "${work_dir}/"
        fi
    done
    sanitize_cargo_config "$work_dir"
    echo "===${marker}-SOURCE-COPY-END==="
    cd "$work_dir"
else
    sanitize_cargo_config "$source_dir"
    cd "$source_dir"
fi

configure_host_rustflags

patch_starry_kallsyms_reserve() {
    linker="os/StarryOS/starryos/linker.ld"

    [ -f "$linker" ] || return

    case "$kallsyms_reserved" in
        *[!0-9KkMmGg]*)
            echo "===${marker}-KALLSYMS-RESERVE-ERROR value=${kallsyms_reserved}==="
            finish_guest 2
            ;;
    esac

    if grep -q '\. += 8M; /\* reserve space for kallsyms' "$linker"; then
        sed -i "s/\\. += 8M; \\/\\* reserve space for kallsyms, can be recycled \\*\\//. += ${kallsyms_reserved}; \\/\\* reserve space for kallsyms, patched by macOS self-build \\*\\//" "$linker"
        echo "===${marker}-KALLSYMS-RESERVE-PATCH value=${kallsyms_reserved} file=${linker}==="
    fi
}

patch_starry_kallsyms_reserve

rustflags="${LINK_RUSTFLAGS:-}"
if [ -n "${EXTRA_RUSTFLAGS:-}" ]; then
    rustflags="${rustflags} ${EXTRA_RUSTFLAGS}"
fi
if [ -n "$rustc_threads" ] && [ "$rustc_threads" != "auto" ]; then
    rustflags="${rustflags} -Zthreads=${rustc_threads}"
fi
export RUSTFLAGS="$rustflags"
build_target_arg="apps/starry/macos-selfbuild/target-aarch64-unknown-none-softfloat-pie.json"
export CARGO_UNSTABLE_JSON_TARGET_SPEC="${CARGO_UNSTABLE_JSON_TARGET_SPEC:-true}"
if [ ! -f "$build_target_arg" ]; then
    echo "===${marker}-TARGET-SPEC-MISSING path=${build_target_arg}==="
    finish_guest 2
fi

echo "===${marker}-ENV-BEGIN==="
echo "jobs=${jobs}"
echo "rayon_num_threads=${RAYON_NUM_THREADS}"
echo "rustc_threads=${rustc_threads}"
echo "profile=${profile}"
echo "build_package=${build_package}"
echo "build_bin=${build_bin}"
echo "build_target=${build_target}"
echo "build_target_arg=${build_target_arg}"
echo "build_std=${build_std}"
echo "build_std_features=${build_std_features}"
echo "features=${features}"
echo "artifact_to_bin=${artifact_to_bin}"
echo "starry_kallsyms_reserved=${STARRY_KALLSYMS_RESERVED}"
echo "cargo_verbose=${cargo_verbose}"
echo "source_dir=${source_dir}"
echo "target_dir=${target_dir}"
echo "artifact_dir=${artifact_dir}"
echo "work_dir=$(pwd)"
echo "cargo_home=${CARGO_HOME}"
echo "libclang_path=${LIBCLANG_PATH:-}"
echo "rustflags=${RUSTFLAGS}"
echo "host_rustflags=-C target-feature=-crt-static"
echo "target_cc=${CC_target_aarch64_unknown_none_softfloat_pie}"
echo "target_ar=${AR_target_aarch64_unknown_none_softfloat_pie}"
echo "target_cflags=${CFLAGS_target_aarch64_unknown_none_softfloat_pie}"
"$RUSTC" --version || true
"$cargo_bin" --version || true
echo "===${marker}-ENV-END==="

set -- "$cargo_bin" build \
    -p "$build_package" \
    --target "$build_target_arg" \
    -Z json-target-spec \
    -Z host-config \
    -Z target-applies-to-host

case "$cargo_verbose" in
    0|"")
        ;;
    1)
        set -- "$@" -v
        ;;
    2)
        set -- "$@" -vv
        ;;
    *)
        echo "===${marker}-CARGO-VERBOSE-ERROR value=${cargo_verbose} expected=0|1|2==="
        finish_guest 2
        ;;
esac

set -- "$@" \
    --bin "$build_bin" \
    -Z "build-std=${build_std}" \
    -Z "build-std-features=${build_std_features}"

set -- "$@" --target-dir "$target_dir"

if [ -n "$features" ] && [ "$features" != "none" ]; then
    set -- "$@" --features "$features"
fi

set -- "$@" --release

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

if [ "$rc" = "0" ]; then
    target_base="${build_target_arg##*/}"
    target_stem="${target_base%.json}"
    artifact="${target_dir}/${target_stem}/release/${build_bin}"

    if [ ! -f "$artifact" ]; then
        echo "===${marker}-ARTIFACT-MISSING path=${artifact}==="
        finish_guest 2
    fi

    run_starry_kallsyms "$artifact"
    bytes="$(wc -c <"$artifact" 2>/dev/null || echo unknown)"
    echo "===${marker}-ARTIFACT path=${artifact} bytes=${bytes}==="
    mkdir -p "$artifact_dir"
    artifact_copy="${artifact_dir}/${build_bin}-${build_target}"
    cp "$artifact" "$artifact_copy"
    sync "$artifact_copy" 2>/dev/null || sync 2>/dev/null || true
    copy_bytes="$(wc -c <"$artifact_copy" 2>/dev/null || echo unknown)"
    echo "===${marker}-ARTIFACT-COPY path=${artifact_copy} bytes=${copy_bytes}==="
    if [ "$artifact_to_bin" = "1" ]; then
        artifact_bin="${artifact}.bin"
        if command -v rust-objcopy >/dev/null 2>&1; then
            rust-objcopy --strip-all -O binary "$artifact" "$artifact_bin"
        elif command -v llvm-objcopy >/dev/null 2>&1; then
            llvm-objcopy --strip-all -O binary "$artifact" "$artifact_bin"
        else
            echo "===${marker}-ARTIFACT-BIN-SKIP reason=objcopy-missing==="
            artifact_bin=""
        fi
        if [ -n "$artifact_bin" ] && [ -f "$artifact_bin" ]; then
            bin_bytes="$(wc -c <"$artifact_bin" 2>/dev/null || echo unknown)"
            echo "===${marker}-ARTIFACT-BIN path=${artifact_bin} bytes=${bin_bytes}==="
            artifact_bin_copy="${artifact_copy}.bin"
            cp "$artifact_bin" "$artifact_bin_copy"
            sync "$artifact_bin_copy" 2>/dev/null || sync 2>/dev/null || true
            bin_copy_bytes="$(wc -c <"$artifact_bin_copy" 2>/dev/null || echo unknown)"
            echo "===${marker}-ARTIFACT-BIN-COPY path=${artifact_bin_copy} bytes=${bin_copy_bytes}==="
        fi
    fi
    echo "===${marker}-PASS jobs=${jobs} elapsed=${elapsed}==="
else
    echo "===${marker}-FAIL jobs=${jobs} rc=${rc} elapsed=${elapsed}==="
fi

finish_guest "$rc"
