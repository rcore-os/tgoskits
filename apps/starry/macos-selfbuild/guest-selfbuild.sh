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
features="${FEATURES:-plat-dyn,ax-driver/virtio-blk,ax-driver/virtio-net,ax-driver/virtio-gpu,ax-driver/virtio-input,ax-driver/virtio-socket,starry-kernel/input,starry-kernel/vsock}"
no_default_features="${NO_DEFAULT_FEATURES:-0}"
allow_slow_selfbuild="${ALLOW_SLOW_SELFBUILD:-0}"
guest_monitor_interval="${GUEST_MONITOR_INTERVAL_SEC:-60}"
target_heartbeat_sec="${TARGET_HEARTBEAT_SEC:-0}"
trace_rustc="${TRACE_RUSTC:-0}"
cargo_verbose="${CARGO_VERBOSE:-0}"
target_spec_mode="${TARGET_SPEC_MODE:-pie}"
target_spec_path="${TARGET_SPEC_PATH:-}"
artifact_to_bin="${ARTIFACT_TO_BIN:-1}"
kallsyms_reserved="${STARRY_KALLSYMS_RESERVED:-64M}"

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

resolve_target_spec() {
    case "$target_spec_mode" in
        none | "")
            printf '%s\n' "$build_target"
            ;;
        pie)
            printf 'scripts/targets/pie/%s.json\n' "$build_target"
            ;;
        no-pie)
            printf 'scripts/targets/no-pie/%s.json\n' "$build_target"
            ;;
        path)
            if [ -z "$target_spec_path" ]; then
                echo "===${marker}-TARGET-SPEC-ERROR mode=path path=empty==="
                finish_guest 2
            fi
            printf '%s\n' "$target_spec_path"
            ;;
        *)
            echo "===${marker}-TARGET-SPEC-ERROR mode=${target_spec_mode} expected=pie|no-pie|path|none==="
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
    "$cargo_bin" install \
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
    if [ ! -f "scripts/axbuild/scripts/starry-kallsyms.sh" ]; then
        echo "===${marker}-KALLSYMS-SCRIPT-MISSING==="
        finish_guest 2
    fi

    ensure_kallsyms_tools
    echo "===${marker}-KALLSYMS-BEGIN elf=${artifact}==="
    set +e
    KERNEL_ELF="$artifact" AXBUILD_STARRY_KALLSYMS_AUTO_INSTALL=0 \
        sh scripts/axbuild/scripts/starry-kallsyms.sh
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
    for path in Cargo.toml Cargo.lock rust-toolchain.toml .tgoskits-source-meta .cargo apps components drivers memory os platforms scripts test-suit tools vendor virtualization xtask; do
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
if [ -n "$rustc_threads" ] && [ "$rustc_threads" != "auto" ]; then
    rustflags="${rustflags} -Zthreads=${rustc_threads}"
fi
export RUSTFLAGS="$rustflags"
build_target_arg="$(resolve_target_spec)"
case "$target_spec_mode" in
    pie | no-pie | path)
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
    pie | no-pie | path)
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
        fi
    fi
    echo "===${marker}-PASS jobs=${jobs} elapsed=${elapsed}==="
else
    echo "===${marker}-FAIL jobs=${jobs} rc=${rc} elapsed=${elapsed}==="
fi

finish_guest "$rc"
