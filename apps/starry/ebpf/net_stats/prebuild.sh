#!/usr/bin/env bash
# Build net_stats (aya loader + embedded eBPF bytecode) as a static musl binary
# and install it into the StarryOS rootfs overlay.
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
overlay_dir="${STARRY_OVERLAY_DIR:?STARRY_OVERLAY_DIR is required}"
arch="${STARRY_ARCH:-x86_64}"

case "$arch" in
    x86_64)      musl_target="x86_64-unknown-linux-musl";      cross_prefix="x86_64-linux-musl" ;;
    aarch64)     musl_target="aarch64-unknown-linux-musl";     cross_prefix="aarch64-linux-musl" ;;
    riscv64)     musl_target="riscv64gc-unknown-linux-musl";   cross_prefix="riscv64-linux-musl" ;;
    loongarch64) musl_target="loongarch64-unknown-linux-musl"; cross_prefix="loongarch64-linux-musl" ;;
    *) echo "net_stats prebuild: unsupported arch '$arch'" >&2; exit 1 ;;
esac

cross_bin="/opt/${cross_prefix}-cross/bin"
[[ -d "$cross_bin" ]] && export PATH="$cross_bin:$PATH"
cc_bin="${cross_prefix}-gcc"

install_loongarch_loader_link() {
    local rootfs="${STARRY_ROOTFS:-}"
    [[ -n "$rootfs" && -f "$rootfs" ]] || {
        echo "net_stats prebuild: STARRY_ROOTFS required for loongarch64 symlink" >&2; exit 1
    }
    debugfs -w "$rootfs" -R "mkdir /lib64" 2>/dev/null || true
    debugfs -w "$rootfs" -R "rm /lib64/ld-musl-loongarch-lp64d.so.1" 2>/dev/null || true
    debugfs -w "$rootfs" -R "symlink /lib64/ld-musl-loongarch-lp64d.so.1 /lib/ld-musl-loongarch64.so.1"
}

echo "net_stats prebuild: building for $musl_target (CC=$cc_bin)"
(
    cd "$app_dir"
    CC="$cc_bin" cargo build --release --target "$musl_target" \
        --config "target.${musl_target}.linker=\"${cc_bin}\""
)

bin="$app_dir/target/$musl_target/release/net_stats"
[[ -x "$bin" ]] || { echo "net_stats prebuild: build did not produce $bin" >&2; exit 1; }

install -Dm0755 "$bin" "$overlay_dir/usr/bin/net_stats"
echo "net_stats prebuild: installed -> /usr/bin/net_stats"

[[ "$arch" == "loongarch64" ]] && install_loongarch_loader_link || true
