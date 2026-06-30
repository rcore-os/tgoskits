#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../../.." && pwd)"

: "${HOST_TOOLS_DIR:=$repo_root/target/starry-macos-selfbuild/host-tools}"
: "${ZIG_CACHE_DIR:=$repo_root/target/starry-macos-selfbuild/zig-cache}"

usage() {
    cat <<'USAGE'
Usage:
  source apps/starry/macos-selfbuild/prepare_host_tools.sh
  prepare_macos_selfbuild_host_tools

  apps/starry/macos-selfbuild/prepare_host_tools.sh

Helper stage: prepares host-side tools needed to build the AArch64 StarryOS
seed kernel on Apple Silicon macOS. If aarch64-linux-musl-{cc,gcc,ar} are
missing and zig is available, local wrappers are generated under
target/starry-macos-selfbuild.

Environment:
  HOST_TOOLS_DIR  Directory for generated host tool wrappers
  ZIG_CACHE_DIR   Directory for zig local/global caches
USAGE
}

zig_lib_dir() {
    zig env 2>/dev/null | sed -n -E '
        s/^[[:space:]]*"lib_dir"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/p
        s/^[[:space:]]*\.lib_dir = "([^"]+)".*/\1/p
    ' | head -1
}

find_archive_tool() {
    if command -v rust-ar >/dev/null 2>&1; then
        command -v rust-ar
        return
    fi

    local sysroot host bin_dir
    sysroot="$(rustc --print sysroot)"
    host="$(rustc -vV | sed -n 's/^host: //p')"
    bin_dir="$sysroot/lib/rustlib/$host/bin"

    if [[ -x "$bin_dir/llvm-ar" ]]; then
        printf '%s\n' "$bin_dir/llvm-ar"
        return
    fi
    if command -v llvm-ar >/dev/null 2>&1; then
        command -v llvm-ar
        return
    fi
    if command -v ar >/dev/null 2>&1; then
        command -v ar
        return
    fi
}

write_archive_wrapper() {
    local wrapper="$HOST_TOOLS_DIR/aarch64-linux-musl-ar"
    local ar_tool
    ar_tool="$(find_archive_tool)"
    if [[ -z "$ar_tool" ]]; then
        echo "failed to find archive tool; tried rust-ar, rust toolchain llvm-ar, llvm-ar, ar" >&2
        exit 1
    fi

    cat >"$wrapper" <<EOF
#!/usr/bin/env bash
set -euo pipefail
exec "$ar_tool" "\$@"
EOF
    chmod +x "$wrapper"
}

ensure_aarch64_musl_gcc() {
    if command -v aarch64-linux-musl-gcc >/dev/null 2>&1 \
        && command -v aarch64-linux-musl-cc >/dev/null 2>&1 \
        && command -v aarch64-linux-musl-ar >/dev/null 2>&1; then
        return
    fi

    mkdir -p "$HOST_TOOLS_DIR"

    if command -v aarch64-linux-musl-gcc >/dev/null 2>&1 \
        || command -v aarch64-linux-musl-cc >/dev/null 2>&1; then
        local cc_path wrapper
        cc_path="$(command -v aarch64-linux-musl-gcc || command -v aarch64-linux-musl-cc)"
        for wrapper in aarch64-linux-musl-gcc aarch64-linux-musl-cc; do
            if ! command -v "$wrapper" >/dev/null 2>&1; then
                cat >"$HOST_TOOLS_DIR/$wrapper" <<EOF
#!/usr/bin/env bash
set -euo pipefail

args=()
while [[ "\$#" -gt 0 ]]; do
    case "\$1" in
        --target=aarch64-linux-musl|--target=aarch64-unknown-linux-musl|--target=aarch64-unknown-none|--target=aarch64-unknown-none-softfloat)
            shift
            ;;
        --target)
            if [[ "\${2:-}" == "aarch64-linux-musl" || "\${2:-}" == "aarch64-unknown-linux-musl" || "\${2:-}" == "aarch64-unknown-none" || "\${2:-}" == "aarch64-unknown-none-softfloat" ]]; then
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

exec "$cc_path" "\${args[@]}"
EOF
                chmod +x "$HOST_TOOLS_DIR/$wrapper"
            fi
        done
    else
        if ! command -v zig >/dev/null 2>&1; then
            echo "aarch64-linux-musl-{cc,gcc} not found; install a musl cross compiler or brew install zig" >&2
            exit 1
        fi

        local zig_path lib_dir musl_include_root musl_generic musl_target wrapper sysroot_view entry
        zig_path="$(command -v zig)"
        lib_dir="$(zig_lib_dir)"
        musl_include_root="$lib_dir/libc/include"
        musl_generic="$musl_include_root/generic-musl"
        musl_target="$musl_include_root/aarch64-linux-musl"
        wrapper="$HOST_TOOLS_DIR/aarch64-linux-musl-gcc"
        sysroot_view="$HOST_TOOLS_DIR/zig-aarch64-musl-sysroot"

        if [[ -z "$lib_dir" || ! -d "$musl_generic" || ! -d "$musl_target/bits" ]]; then
            echo "could not locate zig musl headers under zig lib_dir=$lib_dir" >&2
            exit 1
        fi

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
        --target=aarch64-linux-musl|--target=aarch64-unknown-linux-musl|--target=aarch64-unknown-none|--target=aarch64-unknown-none-softfloat)
            shift
            ;;
        --target)
            if [[ "\${2:-}" == "aarch64-linux-musl" || "\${2:-}" == "aarch64-unknown-linux-musl" || "\${2:-}" == "aarch64-unknown-none" || "\${2:-}" == "aarch64-unknown-none-softfloat" ]]; then
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
        ln -sf "$(basename "$wrapper")" "$HOST_TOOLS_DIR/aarch64-linux-musl-cc"
    fi

    if ! command -v aarch64-linux-musl-ar >/dev/null 2>&1; then
        write_archive_wrapper
    fi
    export PATH="$HOST_TOOLS_DIR:$PATH"
    echo "using local aarch64-linux-musl tool wrappers from $HOST_TOOLS_DIR"
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

ensure_rust_binutils_wrappers() {
    local rust_tool llvm_tool llvm_path wrapper
    mkdir -p "$HOST_TOOLS_DIR"

    for rust_tool in rust-nm rust-objdump; do
        if command -v "$rust_tool" >/dev/null 2>&1 && "$rust_tool" --version >/dev/null 2>&1; then
            continue
        fi

        llvm_tool="${rust_tool#rust-}"
        llvm_tool="llvm-$llvm_tool"
        llvm_path="$(find_llvm_tool "$llvm_tool")"
        wrapper="$HOST_TOOLS_DIR/$rust_tool"

        if [[ -z "$llvm_path" ]]; then
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

prepare_macos_selfbuild_host_tools() {
    ensure_aarch64_musl_gcc
    prepend_rust_objcopy_dir
    ensure_rust_binutils_wrappers
    mkdir -p "$ZIG_CACHE_DIR/local" "$ZIG_CACHE_DIR/global"
    export ZIG_LOCAL_CACHE_DIR="$ZIG_CACHE_DIR/local"
    export ZIG_GLOBAL_CACHE_DIR="$ZIG_CACHE_DIR/global"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
        usage
        exit 0
    fi
    prepare_macos_selfbuild_host_tools
    echo "HOST_TOOLS_DIR=$HOST_TOOLS_DIR"
    echo "ZIG_CACHE_DIR=$ZIG_CACHE_DIR"
fi
