#!/usr/bin/env bash
# Build the `rawtp` eBPF demo (aya loader + embedded bytecode) as a static
# musl binary and install it into the StarryOS rootfs overlay.
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
overlay_dir="${STARRY_OVERLAY_DIR:?STARRY_OVERLAY_DIR is required}"
arch="${STARRY_ARCH:-x86_64}"

case "$arch" in
    x86_64)        musl_target="x86_64-unknown-linux-musl";        cross_prefix="x86_64-linux-musl" ;;
    aarch64)       musl_target="aarch64-unknown-linux-musl";       cross_prefix="aarch64-linux-musl" ;;
    riscv64)       musl_target="riscv64gc-unknown-linux-musl";     cross_prefix="riscv64-linux-musl" ;;
    loongarch64)   musl_target="loongarch64-unknown-linux-musl";   cross_prefix="loongarch64-linux-musl" ;;
    *) echo "rawtp prebuild: unsupported arch '$arch'" >&2; exit 1 ;;
esac

cross_bin="/opt/${cross_prefix}-cross/bin"
if [[ -d "$cross_bin" ]]; then
    export PATH="$cross_bin:$PATH"
fi
cc_bin="${cross_prefix}-gcc"

if ! command -v rustup >/dev/null 2>&1; then
    echo "$(basename "$app_dir") prebuild: rustup is required to install Rust target $musl_target" >&2
    exit 1
fi
read -r rust_toolchain _ < <(rustup show active-toolchain)
echo "$(basename "$app_dir") prebuild: installing Rust target $musl_target for toolchain $rust_toolchain"
rustup target add --toolchain "$rust_toolchain" "$musl_target"
export RUSTUP_TOOLCHAIN="$rust_toolchain"

host_tools_dir="${STARRY_WORKSPACE:-$app_dir}/tmp/axbuild/starry-host-tools"
export PATH="$host_tools_dir/bin:${HOME:-/root}/.cargo/bin:$PATH"
ensure_bpf_linker() {
    if command -v bpf-linker >/dev/null 2>&1; then
        return 0
    fi

    if command -v apk >/dev/null 2>&1; then
        echo "$(basename "$app_dir") prebuild: installing bpf-linker with apk"
        apk add --no-cache bpf-linker || true
        if command -v bpf-linker >/dev/null 2>&1; then
            return 0
        fi
    fi

    echo "$(basename "$app_dir") prebuild: installing bpf-linker 0.10.3 with cargo"
    cargo install bpf-linker --version 0.10.3 --locked --root "$host_tools_dir"
}
ensure_bpf_linker

install_loongarch_loader_link() {
    local rootfs="${STARRY_ROOTFS:-}"
    if [[ -z "$rootfs" || ! -f "$rootfs" ]]; then
        echo "rawtp prebuild: STARRY_ROOTFS is required for loongarch64 loader symlink" >&2
        exit 1
    fi

    echo "rawtp prebuild: installing loongarch64 musl loader compatibility symlink"
    debugfs -w "$rootfs" -R "mkdir /lib64" 2>/dev/null || true
    debugfs -w "$rootfs" -R "rm /lib64/ld-musl-loongarch-lp64d.so.1" 2>/dev/null || true
    debugfs -w "$rootfs" -R "symlink /lib64/ld-musl-loongarch-lp64d.so.1 /lib/ld-musl-loongarch64.so.1"
}

echo "rawtp prebuild: building eBPF demo for $musl_target (CC=$cc_bin)"
(
    cd "$app_dir"
    CC="$cc_bin" cargo build --release --target "$musl_target" \
        --config "target.${musl_target}.linker=\"${cc_bin}\""
)

bin="$app_dir/target/$musl_target/release/rawtp"
[[ -x "$bin" ]] || { echo "rawtp prebuild: build did not produce $bin" >&2; exit 1; }

install -Dm0755 "$bin" "$overlay_dir/usr/bin/rawtp"
echo "rawtp prebuild: installed $(basename "$bin") -> /usr/bin/rawtp"

if [[ "$arch" == "loongarch64" ]]; then
    install_loongarch_loader_link
fi
