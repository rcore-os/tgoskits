#!/usr/bin/env bash
# Build the `upb` uprobe demo (aya loader + embedded eBPF bytecode) as a static
# musl binary and install it into the StarryOS rootfs overlay.
#
# The loader self-attaches a uprobe to its own `uprobe_test` symbol, drives a
# fixed number of calls, then reads the hit count back from a BPF HashMap and
# prints UPROBE_PASS / UPROBE_FAIL. See qemu-x86_64.toml for the assertion.
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
overlay_dir="${STARRY_OVERLAY_DIR:?STARRY_OVERLAY_DIR is required}"
arch="${STARRY_ARCH:-x86_64}"

case "$arch" in
    x86_64)        musl_target="x86_64-unknown-linux-musl";        cross_prefix="x86_64-linux-musl" ;;
    aarch64)       musl_target="aarch64-unknown-linux-musl";       cross_prefix="aarch64-linux-musl" ;;
    riscv64)       musl_target="riscv64gc-unknown-linux-musl";     cross_prefix="riscv64-linux-musl" ;;
    loongarch64)   musl_target="loongarch64-unknown-linux-musl";   cross_prefix="loongarch64-linux-musl" ;;
    *) echo "upb prebuild: unsupported arch '$arch'" >&2; exit 1 ;;
esac

# Make the musl cross toolchain discoverable if it is installed under /opt
# (as in the StarryOS dev container); harmless if the dir is absent.
cross_bin="/opt/${cross_prefix}-cross/bin"
if [[ -d "$cross_bin" ]]; then
    export PATH="$cross_bin:$PATH"
fi
cc_bin="${cross_prefix}-gcc"

install_loongarch_loader_link() {
    local rootfs="${STARRY_ROOTFS:-}"
    if [[ -z "$rootfs" || ! -f "$rootfs" ]]; then
        echo "upb prebuild: STARRY_ROOTFS is required for loongarch64 loader symlink" >&2
        exit 1
    fi

    echo "upb prebuild: installing loongarch64 musl loader compatibility symlink"
    debugfs -w "$rootfs" -R "mkdir /lib64" 2>/dev/null || true
    debugfs -w "$rootfs" -R "rm /lib64/ld-musl-loongarch-lp64d.so.1" 2>/dev/null || true
    debugfs -w "$rootfs" -R "symlink /lib64/ld-musl-loongarch-lp64d.so.1 /lib/ld-musl-loongarch64.so.1"
}

echo "upb prebuild: building eBPF demo for $musl_target (CC=$cc_bin)"
(
    cd "$app_dir"
    CC="$cc_bin" cargo build --release --target "$musl_target" \
        --config "target.${musl_target}.linker=\"${cc_bin}\""
)

bin="$app_dir/target/$musl_target/release/upb"
[[ -x "$bin" ]] || { echo "upb prebuild: build did not produce $bin" >&2; exit 1; }

install -Dm0755 "$bin" "$overlay_dir/usr/bin/upb"
echo "upb prebuild: installed $(basename "$bin") -> /usr/bin/upb"

if [[ "$arch" == "loongarch64" ]]; then
    install_loongarch_loader_link
fi
