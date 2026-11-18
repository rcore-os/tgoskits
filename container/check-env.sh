#!/usr/bin/env bash
# Validate the fixed host environment without downloading or installing tools.
set -euo pipefail

# Docker exports the build argument, but linux-user QEMU interprets this name
# as a request for --version instead of executing the supplied program.
unset QEMU_VERSION

for tool in \
    git git-lfs curl wget tar unzip file perl python3 javac jar go \
    clang ld.lld cmake ninja pkg-config ffmpeg ffprobe \
    debugfs e2fsck resize2fs sfdisk mkfs.vfat mkimage dtc mmd mcopy \
    fakeroot rustc cargo rustup cargo-objcopy bpf-linker verilator iverilog yosys bsc
do
    command -v "$tool" >/dev/null || { echo "missing container tool: $tool" >&2; exit 1; }
done

node --version
npm --version
test "$(node -p 'Number(process.versions.node.split(".")[0]) >= 24')" = true
test "$(npm --version | cut -d. -f1)" -ge 11
python3 -m pip --version
javac -version
go version
bpf-linker --version
verilator --version | grep -F '5.008'
iverilog -V 2>&1 | sed -n '1p'
yosys -V | grep -F '0.58'
bsc -v | sed -n '1p'

temporary="$(mktemp -d)"
trap 'rm -rf "$temporary"' EXIT
printf '#include <stdio.h>\nint main(void) { puts("container-c-ok"); }\n' > "$temporary/hello.c"
printf 'fn main() { println!("container-rust-ok"); }\n' > "$temporary/hello.rs"
installed_targets="$(rustup target list --installed)"
for arch in x86_64 aarch64 riscv64 loongarch64; do
    case "$arch" in
        riscv64) target=riscv64gc-unknown-linux-musl ;;
        *) target="$arch-unknown-linux-musl" ;;
    esac
    printf '%s\n' "$installed_targets" | grep -Fx "$target"
    command -v "qemu-system-$arch"
    command -v "qemu-$arch-static"
    "$arch-linux-musl-gcc" -static "$temporary/hello.c" -o "$temporary/hello-c"
    "qemu-$arch-static" "$temporary/hello-c" | grep -Fx container-c-ok
    # Use a static CRT so the probe can run without a guest rootfs.
    rustc --target "$target" -C target-feature=+crt-static -C "linker=$arch-linux-musl-gcc" \
        "$temporary/hello.rs" -o "$temporary/hello-rust"
    "qemu-$arch-static" "$temporary/hello-rust" | grep -Fx container-rust-ok
done
printf '%s\n' "$installed_targets" | grep -Fx x86_64-unknown-uefi
echo 'Container host prerequisites passed'
