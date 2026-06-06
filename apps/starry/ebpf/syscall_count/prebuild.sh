#!/usr/bin/env bash
# Build the `syscall_count` demo (aya loader + embedded eBPF bytecode) as a
# static musl binary and install it into the StarryOS rootfs overlay.
#
# The loader attaches a kprobe to `syscall::sysno` (whose first argument is the
# raw syscall number, so the read is arch-independent), drives a fixed number of
# getpid(2) calls, then reads the per-syscall hit count back from a BPF HashMap
# and prints SYSCALL_COUNT_PASS / _FAIL.
set -euo pipefail

app_dir="${STARRY_APP_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
overlay_dir="${STARRY_OVERLAY_DIR:?STARRY_OVERLAY_DIR is required}"
arch="${STARRY_ARCH:-x86_64}"

case "$arch" in
    x86_64)        musl_target="x86_64-unknown-linux-musl";        cross_prefix="x86_64-linux-musl" ;;
    aarch64)       musl_target="aarch64-unknown-linux-musl";       cross_prefix="aarch64-linux-musl" ;;
    riscv64)       musl_target="riscv64gc-unknown-linux-musl";     cross_prefix="riscv64-linux-musl" ;;
    loongarch64)   musl_target="loongarch64-unknown-linux-musl";   cross_prefix="loongarch64-linux-musl" ;;
    *) echo "syscall_count prebuild: unsupported arch '$arch'" >&2; exit 1 ;;
esac

cross_bin="/opt/${cross_prefix}-cross/bin"
if [[ -d "$cross_bin" ]]; then
    export PATH="$cross_bin:$PATH"
fi
cc_bin="${cross_prefix}-gcc"

echo "syscall_count prebuild: building eBPF demo for $musl_target (CC=$cc_bin)"
(
    cd "$app_dir"
    CC="$cc_bin" cargo build --release --target "$musl_target" \
        --config "target.${musl_target}.linker=\"${cc_bin}\""
)

bin="$app_dir/target/$musl_target/release/syscall_count"
[[ -x "$bin" ]] || { echo "syscall_count prebuild: build did not produce $bin" >&2; exit 1; }

install -Dm0755 "$bin" "$overlay_dir/usr/bin/syscall_count"
echo "syscall_count prebuild: installed $(basename "$bin") -> /usr/bin/syscall_count"
