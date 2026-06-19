#!/bin/sh
set -eu

marker="${MARKER:-STARRY-MACOS-SELFBUILD}"
jobs="${JOBS:-8}"
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
profile="${PROFILE:-release}"
cargo_subcommand="${CARGO_SUBCOMMAND:-build}"
build_target="${BUILD_TARGET:-aarch64-unknown-none-softfloat}"
build_package="${BUILD_PACKAGE:-starryos}"
build_bin="${BUILD_BIN:-starryos}"
build_std="${BUILD_STD:-core,alloc}"
build_std_features="${BUILD_STD_FEATURES:-compiler-builtins-mem}"
features="${FEATURES:-plat-dyn,ax-driver/virtio-blk,smp}"
no_default_features="${NO_DEFAULT_FEATURES:-1}"
allow_slow_selfbuild="${ALLOW_SLOW_SELFBUILD:-0}"
guest_monitor_interval="${GUEST_MONITOR_INTERVAL_SEC:-60}"
target_heartbeat_sec="${TARGET_HEARTBEAT_SEC:-0}"
trace_rustc="${TRACE_RUSTC:-0}"
cargo_verbose="${CARGO_VERBOSE:-0}"
target_spec_mode="${TARGET_SPEC_MODE:-bare-pie}"
target_spec_path="${TARGET_SPEC_PATH:-}"
artifact_to_bin="${ARTIFACT_TO_BIN:-1}"
kallsyms_reserved="${STARRY_KALLSYMS_RESERVED:-16M}"
artifact_upload_url="${ARTIFACT_UPLOAD_URL:-}"
artifact_upload_token="${ARTIFACT_UPLOAD_TOKEN:-}"
artifact_upload_required="${ARTIFACT_UPLOAD_REQUIRED:-1}"

assert_fast_profile() {
    if [ "$allow_slow_selfbuild" = "1" ]; then
        echo "===${marker}-SLOW-PROFILE-ALLOWED==="
        return
    fi

    if [ "$rustc_threads" != "2" ]; then
        echo "===${marker}-FAST-PROFILE-ERROR rustc_threads=${rustc_threads} expected=2==="
        echo "Set RUSTC_THREADS=2 for the reproducible qemu-aarch64 profile, or ALLOW_SLOW_SELFBUILD=1 for experiments."
        finish_guest 2
    fi

    echo "===${marker}-QEMU-AARCH64-PROFILE expected_crates~420==="
}

std_target_name() {
    case "$1" in
        x86_64-*)
            printf '%s\n' x86_64-unknown-linux-musl
            ;;
        aarch64-*)
            printf '%s\n' aarch64-unknown-linux-musl
            ;;
        riscv64*)
            printf '%s\n' riscv64gc-unknown-linux-musl
            ;;
        loongarch64-*)
            printf '%s\n' loongarch64-unknown-linux-musl
            ;;
        *)
            printf '%s\n' "$1"
            ;;
    esac
}

resolve_target_spec() {
    case "$target_spec_mode" in
        bare-pie)
            printf 'apps/starry/macos-selfbuild/target-aarch64-unknown-none-softfloat-pie.json\n'
            ;;
        none | "")
            printf '%s\n' "$build_target"
            ;;
        pie)
            printf 'scripts/targets/std/pie/%s.json\n' "$(std_target_name "$build_target")"
            ;;
        no-pie)
            printf 'scripts/targets/std/%s.json\n' "$(std_target_name "$build_target")"
            ;;
        path)
            if [ -z "$target_spec_path" ]; then
                echo "===${marker}-TARGET-SPEC-ERROR mode=path path=empty==="
                finish_guest 2
            fi
            printf '%s\n' "$target_spec_path"
            ;;
        *)
            echo "===${marker}-TARGET-SPEC-ERROR mode=${target_spec_mode} expected=bare-pie|pie|no-pie|path|none==="
            finish_guest 2
            ;;
    esac
}

finish_guest() {
    rc="$1"
    if [ -n "${target_heartbeat_pid:-}" ]; then
        kill "$target_heartbeat_pid" 2>/dev/null || true
    fi
    if [ -n "${monitor_pid:-}" ]; then
        kill "$monitor_pid" 2>/dev/null || true
    fi
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

    if [ "$build_package" != "starryos" ] || [ "$build_bin" != "starryos" ]; then
        return
    fi
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

upload_artifact() {
    path="$1"
    label="$2"

    [ -n "$artifact_upload_url" ] || return 0
    if [ ! -s "$path" ]; then
        echo "===${marker}-ARTIFACT-UPLOAD-SKIP label=${label} reason=missing path=${path}==="
        return 0
    fi
    if ! command -v curl >/dev/null 2>&1; then
        echo "===${marker}-ARTIFACT-UPLOAD-FAIL label=${label} reason=curl-missing==="
        [ "$artifact_upload_required" = "1" ] && finish_guest 2
        return 0
    fi

    name="${path##*/}"
    url="${artifact_upload_url%/}/${name}"
    if [ -n "$artifact_upload_token" ]; then
        url="${url}?token=${artifact_upload_token}"
    fi

    echo "===${marker}-ARTIFACT-UPLOAD-BEGIN label=${label} name=${name}==="
    if curl -fsS --retry 3 --connect-timeout 10 --max-time 900 \
        -X PUT --data-binary "@${path}" "$url" >/dev/null; then
        echo "===${marker}-ARTIFACT-UPLOAD-END label=${label} name=${name}==="
    else
        upload_rc="$?"
        echo "===${marker}-ARTIFACT-UPLOAD-FAIL label=${label} name=${name} rc=${upload_rc}==="
        [ "$artifact_upload_required" = "1" ] && finish_guest 2
    fi
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

if [ "$trace_rustc" = "1" ]; then
    real_rustc="$RUSTC"
    trace_dir="/tmp/starry-rustc-trace"
    rm -rf "$trace_dir"
    mkdir -p "$trace_dir"
    cat >"$trace_dir/rustc" <<EOF
#!/bin/sh
real_rustc='$real_rustc'
marker='$marker'
begin="\$(date +%s)"
crate_name=""
target_arg=""
next_crate_name=0
next_target=0
for arg do
    if [ "\$next_crate_name" = "1" ]; then
        crate_name="\$arg"
        next_crate_name=0
    elif [ "\$next_target" = "1" ]; then
        target_arg="\$arg"
        next_target=0
    else
        case "\$arg" in
            --crate-name)
                next_crate_name=1
                ;;
            --crate-name=*)
                crate_name="\${arg#--crate-name=}"
                ;;
            --target)
                next_target=1
                ;;
            --target=*)
                target_arg="\${arg#--target=}"
                ;;
        esac
    fi
done
printf '===%s-RUSTC-BEGIN pid=%s ppid=%s time=%s crate=%s target=%s argv=' "\$marker" "\$\$" "\$PPID" "\$begin" "\${crate_name:-unknown}" "\${target_arg:-unknown}" >&2
for arg do
    printf ' %s' "\$arg" >&2
done
printf '===\n' >&2
"\$real_rustc" "\$@"
rc="\$?"
end="\$(date +%s)"
elapsed="\$((end - begin))"
echo "===\${marker}-RUSTC-END pid=\$\$ rc=\${rc} elapsed=\${elapsed} crate=\${crate_name:-unknown} target=\${target_arg:-unknown}===" >&2
exit "\$rc"
EOF
    chmod +x "$trace_dir/rustc"
    export RUSTC="$trace_dir/rustc"
fi

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

sanitize_cargo_config() {
    config_dir="$1"
    if [ -f "${config_dir}/.cargo/config.toml" ]; then
        sed -i "s#${source_dir}/vendor#${config_dir}/vendor#g" "${config_dir}/.cargo/config.toml" || true
        sed -i '/^include[[:space:]]*=/d' "${config_dir}/.cargo/config.toml" || true
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

if [ "${USE_PREGENERATED_CTYPES:-1}" = "1" ] \
    && [ -f os/arceos/api/arceos_posix_api/src/ctypes_gen.rs ] \
    && [ -d os/arceos/ulib/axlibc/include ]; then
    mv os/arceos/ulib/axlibc/include os/arceos/ulib/axlibc/include.selfbuild-disabled
    echo "===${marker}-PREGENERATED-CTYPES enabled=1==="
fi

patch_lwprintf_rs() {
    binding="/opt/starry-macos-lwprintf.rs"
    [ "${USE_PREGENERATED_LWPRINTF:-1}" = "1" ] || return
    [ -f "$binding" ] || return

    for crate_dir in /root/.cargo/registry/src/*/lwprintf-rs-0.3.3; do
        [ -d "$crate_dir" ] || continue
        cp "$binding" "$crate_dir/src/lwprintf.rs"
        if [ ! -f "$crate_dir/build.rs.selfbuild-orig" ]; then
            cp "$crate_dir/build.rs" "$crate_dir/build.rs.selfbuild-orig"
        fi
        cat >"$crate_dir/build.rs" <<'EOF'
use std::{env, fs, path::PathBuf, process::Command};

use cc::Build;

fn main() {
    build_lib();
    copy_pregenerated_bindings();
}

fn set_arch_flags(builder: &mut Build) {
    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();

    match arch.as_str() {
        "aarch64" => {
            builder.flag("-mgeneral-regs-only");
        }
        "riscv64" => {
            builder.flag_if_supported("-march=rv64gc");
            builder.flag_if_supported("-mabi=lp64d");
            builder.flag_if_supported("-mcmodel=medany");
        }
        "x86_64" => {
            builder.flag_if_supported("-mno-sse");
        }
        "loongarch64" => {
            builder.flag_if_supported("-msoft-float");
        }
        _ => panic!("Unsupported architecture: {}", arch),
    }
}

fn build_lib() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let c_src = manifest_dir.join("./lwprintf/lwprintf/src/lwprintf/lwprintf.c");
    let include_dir = manifest_dir.join("./lwprintf/lwprintf/src/include");
    let opts_file = manifest_dir.join("lwprintf_opts.h");

    println!("cargo:rerun-if-changed={}", c_src.display());
    println!("cargo:rerun-if-changed={}", opts_file.display());
    println!(
        "cargo:rerun-if-changed={}",
        include_dir.join("lwprintf/lwprintf.h").display()
    );

    let os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    let libc_env = env::var("CARGO_CFG_TARGET_ENV").unwrap();
    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();

    let mut builder = cc::Build::new();
    builder
        .file(&c_src)
        .include(&include_dir)
        .include(&manifest_dir)
        .flags([
            "-std=gnu99",
            "-fdata-sections",
            "-ffunction-sections",
            "-fPIC",
            "-fno-builtin",
            "-ffreestanding",
            "-fno-omit-frame-pointer",
        ])
        .warnings(true);

    if os == "none" {
        let musl_gcc = format!("{}-linux-musl-gcc", arch);
        set_arch_flags(&mut builder);
        builder.compiler(&musl_gcc);
        add_sysroot_include(&mut builder, &musl_gcc);
    } else if arch == "loongarch64" && libc_env == "musl" {
        let musl_gcc = format!("{}-linux-musl-gcc", arch);
        add_sysroot_include(&mut builder, &musl_gcc);
    }

    builder.compile("lwprintf");
}

fn add_sysroot_include(builder: &mut Build, cc: &str) {
    let output = Command::new(cc)
        .args(["-print-sysroot"])
        .output()
        .expect("failed to execute process: gcc -print-sysroot");
    let sysroot = core::str::from_utf8(&output.stdout).unwrap().trim_end();
    if !sysroot.is_empty() {
        builder.include(format!("{sysroot}/include"));
    }
}

fn copy_pregenerated_bindings() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let src = manifest_dir.join("src/lwprintf.rs");
    let dst = out_dir.join("lwprintf.rs");
    println!("cargo:rerun-if-changed={}", src.display());
    fs::copy(&src, &dst).expect("failed to copy pregenerated lwprintf.rs");
}
EOF
        echo "===${marker}-PREGENERATED-LWPRINTF crate=${crate_dir}==="
    done
}

patch_lwprintf_rs

patch_starry_kallsyms_reserve() {
    linker="os/StarryOS/starryos/linker.ld"

    [ "$build_package" = "starryos" ] || return
    [ "$build_bin" = "starryos" ] || return
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

if [ -n "${AX_CONFIG_PATH:-}" ]; then
    export AX_CONFIG_PATH
elif [ -f "$(pwd)/os/StarryOS/.axconfig.toml" ]; then
    export AX_CONFIG_PATH="$(pwd)/os/StarryOS/.axconfig.toml"
else
    unset AX_CONFIG_PATH
fi

case "$target_spec_mode" in
    none | "")
        default_link_rustflags="-Clink-arg=-Tlinker.x -Clink-arg=-no-pie -Clink-arg=-znostart-stop-gc"
        ;;
    *)
        default_link_rustflags=""
        ;;
esac
rustflags="${LINK_RUSTFLAGS:-$default_link_rustflags}"
if [ -n "${EXTRA_RUSTFLAGS:-}" ]; then
    rustflags="${rustflags} ${EXTRA_RUSTFLAGS}"
fi
if [ -n "$rustc_threads" ] && [ "$rustc_threads" != "auto" ]; then
    rustflags="${rustflags} -Zthreads=${rustc_threads}"
fi
export RUSTFLAGS="$rustflags"
build_target_arg="$(resolve_target_spec)"
case "$target_spec_mode" in
    bare-pie | pie | no-pie | path)
        export CARGO_UNSTABLE_JSON_TARGET_SPEC="${CARGO_UNSTABLE_JSON_TARGET_SPEC:-true}"
        if [ ! -f "$build_target_arg" ]; then
            echo "===${marker}-TARGET-SPEC-MISSING path=${build_target_arg}==="
            finish_guest 2
        fi
        ;;
esac

echo "===${marker}-ENV-BEGIN==="
echo "jobs=${jobs}"
echo "rayon_num_threads=${RAYON_NUM_THREADS}"
echo "rustc_threads=${rustc_threads}"
echo "profile=${profile}"
echo "cargo_subcommand=${cargo_subcommand}"
echo "build_package=${build_package}"
echo "build_bin=${build_bin}"
echo "build_target=${build_target}"
echo "build_target_arg=${build_target_arg}"
echo "target_spec_mode=${target_spec_mode}"
echo "build_std=${build_std}"
echo "build_std_features=${build_std_features}"
echo "features=${features}"
echo "artifact_to_bin=${artifact_to_bin}"
echo "starry_kallsyms_reserved=${STARRY_KALLSYMS_RESERVED}"
echo "allow_slow_selfbuild=${allow_slow_selfbuild}"
echo "guest_monitor_interval_sec=${guest_monitor_interval}"
echo "trace_rustc=${trace_rustc}"
echo "cargo_verbose=${cargo_verbose}"
echo "source_dir=${source_dir}"
echo "target_dir=${target_dir}"
echo "artifact_dir=${artifact_dir}"
echo "artifact_upload_url=${artifact_upload_url:-}"
echo "target_heartbeat_sec=${target_heartbeat_sec}"
echo "work_dir=$(pwd)"
echo "cargo_home=${CARGO_HOME}"
echo "ax_config_path=${AX_CONFIG_PATH:-}"
echo "libclang_path=${LIBCLANG_PATH:-}"
echo "rustflags=${RUSTFLAGS}"
"$RUSTC" --version || true
"$cargo_bin" --version || true
echo "===${marker}-ENV-END==="

assert_fast_profile

target_heartbeat_pid=""
if [ "$target_heartbeat_sec" != "0" ]; then
    (
        target_heartbeat_mark="/tmp/${marker}-target-heartbeat.mark"
        : >"$target_heartbeat_mark" 2>/dev/null || true
        while :; do
            sleep "$target_heartbeat_sec"
            target_kib="$(du -sk "$target_dir" 2>/dev/null | awk '{ print $1 }' || true)"
            target_files="$(find "$target_dir" -type f 2>/dev/null | wc -l | tr -d ' ' || true)"
            target_changed_files="$(find "$target_dir" -type f -newer "$target_heartbeat_mark" 2>/dev/null | wc -l | tr -d ' ' || true)"
            target_changed_sample="$(find "$target_dir" -type f -newer "$target_heartbeat_mark" 2>/dev/null | tail -n 3 | tr '\n' ',' | sed 's/,$//' || true)"
            touch "$target_heartbeat_mark" 2>/dev/null || true
            echo "===${marker}-TARGET-HEARTBEAT dir=${target_dir} kib=${target_kib:-unknown} files=${target_files:-unknown} changed=${target_changed_files:-unknown} sample=${target_changed_sample:-none}==="
        done
    ) &
    target_heartbeat_pid="$!"
fi

set -- "$cargo_bin" "$cargo_subcommand" \
    -p "$build_package" \
    --target "$build_target_arg"

case "$target_spec_mode" in
    bare-pie | pie | no-pie | path)
        set -- "$@" -Z json-target-spec
        ;;
esac

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

if [ -n "$build_bin" ] && [ "$build_bin" != "none" ]; then
    set -- "$@" --bin "$build_bin"
fi

if [ -n "$build_std" ] && [ "$build_std" != "none" ]; then
    set -- "$@" -Z "build-std=${build_std}"
fi

if [ -n "$build_std_features" ] && [ "$build_std_features" != "none" ]; then
    set -- "$@" -Z "build-std-features=${build_std_features}"
fi

set -- "$@" --target-dir "$target_dir"

if [ "$no_default_features" = "1" ]; then
    set -- "$@" --no-default-features
fi

if [ -n "$features" ] && [ "$features" != "none" ]; then
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
cargo_pid=""
monitor_pid=""

"$@" &
cargo_pid="$!"

if [ "$guest_monitor_interval" != "0" ]; then
    (
        while kill -0 "$cargo_pid" 2>/dev/null; do
            echo "===${marker}-GUEST-PROCS timestamp=$(date +%s) cargo_pid=${cargo_pid}==="
            ps -ef 2>/dev/null || ps 2>/dev/null || true
            echo "===${marker}-GUEST-PROC-STAT-BEGIN==="
            for stat in /proc/[0-9]*/stat; do
                [ -r "$stat" ] || continue
                read -r pid comm state ppid rest <"$stat" || continue
                echo "pid=${pid} ppid=${ppid} state=${state} comm=${comm}"
                case "$comm" in
                    "(cargo)"|"(rustc)"|"(ld-musl-aarch64.)")
                        if [ -r "/proc/${pid}/cmdline" ]; then
                            tr '\000' ' ' <"/proc/${pid}/cmdline" 2>/dev/null || true
                            echo
                        fi
                        if [ -r "/proc/${pid}/status" ]; then
                            sed -n '1,30p' "/proc/${pid}/status" 2>/dev/null || true
                        fi
                        if [ -r "/proc/${pid}/wchan" ]; then
                            printf 'wchan='
                            cat "/proc/${pid}/wchan" 2>/dev/null || true
                            echo
                        fi
                        if [ -d "/proc/${pid}/task" ]; then
                            echo "===${marker}-GUEST-TASKS pid=${pid} comm=${comm}==="
                            for task_stat in /proc/"${pid}"/task/[0-9]*/stat; do
                                [ -r "$task_stat" ] || continue
                                read -r tid task_comm task_state task_ppid task_rest <"$task_stat" || continue
                                echo "tid=${tid} ppid=${task_ppid} state=${task_state} comm=${task_comm}"
                            done
                        fi
                        ;;
                esac
            done
            echo "===${marker}-GUEST-PROC-STAT-END==="
            if [ -d "/proc/${cargo_pid}/task" ]; then
                echo "===${marker}-GUEST-CARGO-TASKS-BEGIN==="
                for stat in /proc/"${cargo_pid}"/task/[0-9]*/stat; do
                    [ -r "$stat" ] || continue
                    read -r tid comm state ppid rest <"$stat" || continue
                    echo "tid=${tid} ppid=${ppid} state=${state} comm=${comm}"
                done
                echo "===${marker}-GUEST-CARGO-TASKS-END==="
            fi
            echo "===${marker}-GUEST-PROCS-END==="
            sleep "$guest_monitor_interval"
        done
    ) &
    monitor_pid="$!"
fi

wait "$cargo_pid"
rc="$?"
if [ -n "$monitor_pid" ]; then
    kill "$monitor_pid" 2>/dev/null || true
    wait "$monitor_pid" 2>/dev/null || true
fi
set -e
if [ -n "$target_heartbeat_pid" ]; then
    kill "$target_heartbeat_pid" 2>/dev/null || true
    target_heartbeat_pid=""
fi

end="$(date +%s)"
elapsed="$((end - start))"
echo "===${marker}-END jobs=${jobs} rc=${rc} elapsed=${elapsed}==="

if [ "$rc" = "0" ]; then
    if [ -n "$build_bin" ] && [ "$build_bin" != "none" ]; then
        target_stem="$build_target"
        case "$build_target_arg" in
            */*.json)
                target_base="${build_target_arg##*/}"
                target_stem="${target_base%.json}"
                ;;
        esac
        if [ "$profile" = "release" ]; then
            artifact="${target_dir}/${target_stem}/release/${build_bin}"
        else
            artifact="${target_dir}/${target_stem}/debug/${build_bin}"
        fi

        if [ -f "$artifact" ]; then
            run_starry_kallsyms "$artifact"
            bytes="$(wc -c <"$artifact" 2>/dev/null || echo unknown)"
            echo "===${marker}-ARTIFACT path=${artifact} bytes=${bytes}==="
            mkdir -p "$artifact_dir"
            artifact_copy="${artifact_dir}/${build_bin}-${build_target}"
            cp "$artifact" "$artifact_copy"
            sync "$artifact_copy" 2>/dev/null || sync 2>/dev/null || true
            copy_bytes="$(wc -c <"$artifact_copy" 2>/dev/null || echo unknown)"
            echo "===${marker}-ARTIFACT-COPY path=${artifact_copy} bytes=${copy_bytes}==="
            upload_artifact "$artifact_copy" "elf"
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
                    upload_artifact "$artifact_bin_copy" "bin"
                fi
            fi
        fi
    fi
    echo "===${marker}-PASS jobs=${jobs} elapsed=${elapsed}==="
else
    echo "===${marker}-FAIL jobs=${jobs} rc=${rc} elapsed=${elapsed}==="
fi

finish_guest "$rc"
