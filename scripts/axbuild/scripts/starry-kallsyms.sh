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
    command -v rust-objdump >/dev/null 2>&1
    command -v gen_ksym >/dev/null 2>&1
}

kallsyms_section_size() {
    section_hex=$(rust-objdump -h "$KERNEL_ELF" | awk "\$2 == \".kallsyms\" { print \$3; found = 1 } END { if (!found) exit 1 }")
    printf "%d\n" "0x$section_hex"
}

pad_kallsyms_to_section() {
    section_size="$1"
    kallsyms_size=$(wc -c < "$kallsyms" | tr -d ' ')
    if [ "$kallsyms_size" -gt "$section_size" ]; then
        echo "generated kallsyms (${kallsyms_size} bytes) exceed .kallsyms section (${section_size} bytes)" >&2
        echo "remove the stale kernel ELF or rebuild it so the linker script reserve is restored" >&2
        exit 1
    fi

    if [ "$kallsyms_size" -lt "$section_size" ]; then
        dd if=/dev/zero bs=1 count=$((section_size - kallsyms_size)) >> "$kallsyms" 2>/dev/null
    fi
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

    section_size=$(kallsyms_section_size)
    pad_kallsyms_to_section "$section_size"
    rust-objcopy --update-section .kallsyms="$kallsyms" "$KERNEL_ELF"
}

refresh_bin_if_present() {
    bin="${KERNEL_ELF%.elf}.bin"
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
