#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../../.." && pwd)"

config="${CONFIG:-$script_dir/build-aarch64-unknown-none-softfloat.toml}"
host_tools_dir="${HOST_TOOLS_DIR:-$repo_root/target/starry-macos-selfbuild/host-tools}"
zig_cache_dir="${ZIG_CACHE_DIR:-$repo_root/target/starry-macos-selfbuild/zig-cache}"

usage() {
    cat <<'USAGE'
Usage:
  apps/starry/macos-selfbuild/build_kernel.sh [extra cargo xtask starry build args]

Builds the AArch64 StarryOS seed kernel used by the macOS HVF self-build run.
On macOS, lwprintf-rs expects an aarch64-linux-musl-gcc binary while compiling
bare-metal C helpers. If that binary is missing and zig is available, this
script creates a local wrapper under target/starry-macos-selfbuild/host-tools.

Environment:
  CONFIG          Build config path
  HOST_TOOLS_DIR  Directory for generated host tool wrappers
  ZIG_CACHE_DIR   Directory for zig local/global caches
USAGE
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
    usage
    exit 0
fi

zig_lib_dir() {
    zig env 2>/dev/null | sed -n -E '
        s/^[[:space:]]*"lib_dir"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/p
        s/^[[:space:]]*\.lib_dir = "([^"]+)".*/\1/p
    ' | head -1
}

ensure_aarch64_musl_gcc() {
    if command -v aarch64-linux-musl-gcc >/dev/null 2>&1; then
        return
    fi

    if ! command -v zig >/dev/null 2>&1; then
        echo "aarch64-linux-musl-gcc not found; install a musl cross gcc or brew install zig" >&2
        exit 1
    fi

    local zig_path lib_dir musl_include_root musl_generic musl_target wrapper cc_wrapper ar_wrapper sysroot_view llvm_ar
    zig_path="$(command -v zig)"
    lib_dir="$(zig_lib_dir)"
    musl_include_root="$lib_dir/libc/include"
    musl_generic="$musl_include_root/generic-musl"
    musl_target="$musl_include_root/aarch64-linux-musl"
    wrapper="$host_tools_dir/aarch64-linux-musl-gcc"
    cc_wrapper="$host_tools_dir/aarch64-linux-musl-cc"
    ar_wrapper="$host_tools_dir/aarch64-linux-musl-ar"
    sysroot_view="$host_tools_dir/zig-aarch64-musl-sysroot"
    llvm_ar="$(find_llvm_tool llvm-ar)"

    if [[ -z "$lib_dir" || ! -d "$musl_generic" || ! -d "$musl_target/bits" ]]; then
        echo "could not locate zig musl headers under zig lib_dir=$lib_dir" >&2
        exit 1
    fi
    if [[ -z "$llvm_ar" ]]; then
        echo "llvm-ar is unavailable; install llvm-tools-preview or brew install llvm" >&2
        exit 1
    fi

    mkdir -p "$host_tools_dir"
    rm -rf "$sysroot_view"
    mkdir -p "$sysroot_view/include"
    for entry in "$musl_generic"/*; do
        ln -s "$entry" "$sysroot_view/include/$(basename "$entry")"
    done
    rm -f "$sysroot_view/include/bits"
    mkdir -p "$sysroot_view/include/bits"
    for entry in "$musl_generic/bits"/*; do
        ln -s "$entry" "$sysroot_view/include/bits/$(basename "$entry")"
    done
    for entry in "$musl_target/bits"/*; do
        rm -f "$sysroot_view/include/bits/$(basename "$entry")"
        ln -s "$entry" "$sysroot_view/include/bits/$(basename "$entry")"
    done

    cat >"$wrapper" <<EOF
#!/usr/bin/env bash
set -euo pipefail

if [[ "\${1:-}" == "-print-sysroot" ]]; then
    printf '%s\n' "$sysroot_view"
    exit 0
fi

args=()
while [[ "\$#" -gt 0 ]]; do
    case "\$1" in
        --target=aarch64-unknown-none|--target=aarch64-unknown-none-softfloat|--target=aarch64-unknown-linux-musl)
            shift
            ;;
        --target)
            if [[ "\${2:-}" == "aarch64-unknown-none" || "\${2:-}" == "aarch64-unknown-none-softfloat" || "\${2:-}" == "aarch64-unknown-linux-musl" ]]; then
                shift 2
            else
                args+=("\$1")
                shift
            fi
            ;;
        *)
            args+=("\$1")
            shift
            ;;
    esac
done

exec "$zig_path" cc -target aarch64-linux-musl "\${args[@]}"
EOF
    chmod +x "$wrapper"
    ln -sf "$(basename "$wrapper")" "$cc_wrapper"
    cat >"$ar_wrapper" <<EOF
#!/usr/bin/env bash
set -euo pipefail
exec "$llvm_ar" "\$@"
EOF
    chmod +x "$ar_wrapper"
    export PATH="$host_tools_dir:$PATH"
    echo "using zig-backed aarch64-linux-musl wrappers in: $host_tools_dir"
}

prepend_rust_objcopy_dir() {
    local sysroot host bin_dir
    sysroot="$(rustc --print sysroot)"
    host="$(rustc -vV | sed -n 's/^host: //p')"
    bin_dir="$sysroot/lib/rustlib/$host/bin"

    if [[ -x "$bin_dir/rust-objcopy" ]]; then
        export PATH="$bin_dir:$PATH"
    fi
}

find_llvm_tool() {
    local name="$1"
    if command -v "$name" >/dev/null 2>&1; then
        command -v "$name"
        return
    fi

    for candidate in \
        "/opt/homebrew/opt/llvm/bin/$name" \
        "/opt/homebrew/opt/llvm@21/bin/$name" \
        "/opt/homebrew/opt/llvm@20/bin/$name"; do
        if [[ -x "$candidate" ]]; then
            printf '%s\n' "$candidate"
            return
        fi
    done
}

quote_toml_string() {
    local value="$1"
    value="${value//\\/\\\\}"
    value="${value//\"/\\\"}"
    printf '"%s"' "$value"
}

config_with_extra_features() {
    local source_config="$1"
    local extra_csv="$2"
    local output="$host_tools_dir/$(basename "$source_config" .toml)-extra-features.toml"
    local -a extra_features=()
    local feature line in_features=0 inserted=0

    IFS=',' read -r -a extra_features <<<"$extra_csv"
    mkdir -p "$host_tools_dir"
    : >"$output"

    while IFS= read -r line || [[ -n "$line" ]]; do
        if [[ "$in_features" = "1" && "$line" =~ ^[[:space:]]*] ]]; then
            if [[ "$inserted" = "0" ]]; then
                for feature in "${extra_features[@]}"; do
                    feature="${feature#"${feature%%[![:space:]]*}"}"
                    feature="${feature%"${feature##*[![:space:]]}"}"
                    if [[ -n "$feature" ]]; then
                        printf '  %s,\n' "$(quote_toml_string "$feature")" >>"$output"
                    fi
                done
                inserted=1
            fi
            in_features=0
        fi

        printf '%s\n' "$line" >>"$output"

        if [[ "$line" =~ ^[[:space:]]*features[[:space:]]*=.*\[[[:space:]]*$ ]]; then
            in_features=1
        fi
    done <"$source_config"

    if [[ "$inserted" = "0" ]]; then
        printf '\nfeatures = [\n' >>"$output"
        for feature in "${extra_features[@]}"; do
            feature="${feature#"${feature%%[![:space:]]*}"}"
            feature="${feature%"${feature##*[![:space:]]}"}"
            if [[ -n "$feature" ]]; then
                printf '  %s,\n' "$(quote_toml_string "$feature")" >>"$output"
            fi
        done
        printf ']\n' >>"$output"
    fi

    printf '%s\n' "$output"
}

ensure_rust_binutils_wrappers() {
    local rust_tool llvm_tool llvm_path wrapper
    mkdir -p "$host_tools_dir"

    for rust_tool in rust-ar rust-nm rust-objcopy rust-objdump; do
        llvm_tool="${rust_tool#rust-}"
        llvm_tool="llvm-$llvm_tool"
        llvm_path="$(find_llvm_tool "$llvm_tool")"
        wrapper="$host_tools_dir/$rust_tool"

        if [[ -z "$llvm_path" ]]; then
            if command -v "$rust_tool" >/dev/null 2>&1 && "$rust_tool" --version >/dev/null 2>&1; then
                continue
            fi
            echo "$rust_tool is unavailable; install llvm-tools-preview or brew install llvm" >&2
            exit 1
        fi

        cat >"$wrapper" <<EOF
#!/usr/bin/env bash
set -euo pipefail
exec "$llvm_path" "\$@"
EOF
        chmod +x "$wrapper"
    done
}

ensure_ostool_llvm_objcopy() {
    local sysroot host bin_dir expected source
    sysroot="$(rustc --print sysroot)"
    host="$(rustc -vV | sed -n 's/^host: //p')"
    bin_dir="$sysroot/lib/rustlib/$host/bin"
    expected="$bin_dir/llvm-objcopy"

    if [[ -x "$expected" ]]; then
        return
    fi

    if [[ -x "$bin_dir/rust-objcopy" ]]; then
        source="$bin_dir/rust-objcopy"
    else
        source="$(find_llvm_tool llvm-objcopy)"
    fi
    if [[ -z "$source" || ! -x "$source" ]]; then
        echo "llvm-objcopy is unavailable; install llvm-tools-preview or brew install llvm" >&2
        exit 1
    fi
    if ! ln -sf "$source" "$expected" 2>/dev/null; then
        cat >&2 <<EOF
failed to create $expected -> $source
ostool expects llvm-objcopy at that exact Rust toolchain path.
Install/repair the component manually:

  rustup component add llvm-tools-preview

or make the toolchain bin directory writable and rerun this script.
EOF
        exit 1
    fi
    echo "using llvm-objcopy compatibility link: $expected -> $source"
}

ensure_aarch64_musl_gcc
prepend_rust_objcopy_dir
ensure_rust_binutils_wrappers
ensure_ostool_llvm_objcopy
mkdir -p "$zig_cache_dir/local" "$zig_cache_dir/global"
export ZIG_LOCAL_CACHE_DIR="$zig_cache_dir/local"
export ZIG_GLOBAL_CACHE_DIR="$zig_cache_dir/global"

cd "$repo_root"
if [[ -n "${STARRY_KERNEL_EXTRA_FEATURES:-}" ]]; then
    config="$(config_with_extra_features "$config" "$STARRY_KERNEL_EXTRA_FEATURES")"
    echo "using extra Starry kernel features from config: $config"
fi
cargo_bin="$(rustc --print sysroot)/bin/cargo"
if [[ ! -x "$cargo_bin" ]]; then
    cargo_bin="$(command -v cargo)"
fi
export PATH="$host_tools_dir:$PATH"
"$cargo_bin" xtask starry build -c "$config" "$@"

actual_bin="$repo_root/target/aarch64-unknown-linux-musl/release/starryos.bin"
default_bin="$repo_root/target/aarch64-unknown-none-softfloat/release/starryos.bin"
if [[ -f "$actual_bin" && "$actual_bin" != "$default_bin" ]]; then
    mkdir -p "$(dirname "$default_bin")"
    cp "$actual_bin" "$default_bin"
    echo "kernel=$default_bin"
fi
