#!/usr/bin/env bash
# Build the `kret` eBPF demo (aya loader + embedded bytecode) as a static musl
# binary and install it into the StarryOS rootfs overlay.
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
overlay_dir="${STARRY_OVERLAY_DIR:?STARRY_OVERLAY_DIR is required}"
arch="${STARRY_ARCH:-x86_64}"

case "$arch" in
    x86_64)        musl_target="x86_64-unknown-linux-musl";        cross_prefix="x86_64-linux-musl" ;;
    aarch64)       musl_target="aarch64-unknown-linux-musl";       cross_prefix="aarch64-linux-musl" ;;
    riscv64)       musl_target="riscv64gc-unknown-linux-musl";     cross_prefix="riscv64-linux-musl" ;;
    loongarch64)   musl_target="loongarch64-unknown-linux-musl";   cross_prefix="loongarch64-linux-musl" ;;
    *) echo "kret prebuild: unsupported arch '$arch'" >&2; exit 1 ;;
esac

cross_bin="/opt/${cross_prefix}-cross/bin"
if [[ -d "$cross_bin" ]]; then
    export PATH="$cross_bin:$PATH"
fi
cc_bin="${cross_prefix}-gcc"

echo "kret prebuild: building eBPF demo for $musl_target (CC=$cc_bin)"
(
    cd "$app_dir"
    CC="$cc_bin" cargo build --release --target "$musl_target" \
        --config "target.${musl_target}.linker=\"${cc_bin}\""
)

bin="$app_dir/target/$musl_target/release/kret"
[[ -x "$bin" ]] || { echo "kret prebuild: build did not produce $bin" >&2; exit 1; }

install -Dm0755 "$bin" "$overlay_dir/usr/bin/kret"
echo "kret prebuild: installed $(basename "$bin") -> /usr/bin/kret"
