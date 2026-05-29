#!/usr/bin/env sh
set -eu

auto_install_enabled() {
    case "${AXBUILD_STARRY_KALLSYMS_AUTO_INSTALL:-1}" in
        0 | n | no | false | off) return 1 ;;
        *) return 0 ;;
    esac
}

install_rust_binutils() {
    if ! auto_install_enabled; then
        echo "rust-nm and rust-objcopy are required for Starry kallsyms generation" >&2
        echo "install them with: rustup component add llvm-tools-preview && cargo install cargo-binutils" >&2
        exit 1
    fi

    echo "Installing rust-nm and rust-objcopy (cargo-binutils)..." >&2
    if command -v rustup >/dev/null 2>&1; then
        rustup component add llvm-tools-preview
    fi
    cargo install cargo-binutils
}

ensure_llvm_tools() {
    if command -v rust-nm >/dev/null 2>&1 \
        && command -v rust-objcopy >/dev/null 2>&1 \
        && rust-nm --version >/dev/null 2>&1 \
        && rust-objcopy --version >/dev/null 2>&1; then
        return
    fi

    if ! command -v rustup >/dev/null 2>&1; then
        return
    fi
    if rustup component list --installed | grep -q '^llvm-tools'; then
        return
    fi

    if ! auto_install_enabled; then
        echo "llvm-tools-preview is required for rust-nm and rust-objcopy" >&2
        echo "install it with: rustup component add llvm-tools-preview" >&2
        exit 1
    fi

    echo "Installing llvm-tools-preview via rustup..." >&2
    rustup component add llvm-tools-preview
}

install_ksym() {
    if ! auto_install_enabled; then
        echo "gen_ksym is required for Starry kallsyms generation" >&2
        echo "install it with: cargo install ksym" >&2
        exit 1
    fi

    echo "Installing ksym (gen_ksym) via cargo..." >&2
    cargo install ksym
}

ensure_tools() {
    ensure_llvm_tools

    if ! command -v rust-nm >/dev/null 2>&1 || ! command -v rust-objcopy >/dev/null 2>&1; then
        install_rust_binutils
    fi
    if ! command -v gen_ksym >/dev/null 2>&1; then
        install_ksym
    fi

    command -v rust-nm >/dev/null 2>&1
    command -v rust-objcopy >/dev/null 2>&1
    command -v gen_ksym >/dev/null 2>&1
}

generate_kallsyms() {
    symbols=$(mktemp "${KERNEL_ELF}.symbols.XXXXXX")
    kallsyms=$(mktemp "${KERNEL_ELF}.kallsyms.XXXXXX")
    trap 'rm -f "$symbols" "$kallsyms"' EXIT

    rust-nm -n "$KERNEL_ELF" > "$symbols"
    grep ' [TtDBR] ' "$symbols" \
        | awk '$3 !~ /^\.L/' \
        | awk '$3 != "$x"' \
        | gen_ksym > "$kallsyms"

    rust-objcopy --update-section .kallsyms="$kallsyms" "$KERNEL_ELF"
}

refresh_bin_if_present() {
    base="$KERNEL_ELF"
    case "$KERNEL_ELF" in
        *.elf) base="${KERNEL_ELF%.elf}" ;;
    esac
    bin="$base.bin"
    if [ -f "$bin" ]; then
        rust-objcopy --strip-all -O binary "$KERNEL_ELF" "$bin"
    fi
}

if [ -z "${KERNEL_ELF:-}" ]; then
    echo "KERNEL_ELF is required for Starry kallsyms generation" >&2
    exit 1
fi

ensure_tools
generate_kallsyms
refresh_bin_if_present
