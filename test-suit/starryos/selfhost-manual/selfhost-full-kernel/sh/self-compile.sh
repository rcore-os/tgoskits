#!/usr/bin/bash
set -euo pipefail

echo "SELFHOST_START"
export PATH=/root/.cargo/bin:/usr/local/bin:$PATH
export RUSTUP_HOME=/root/.rustup
export CARGO_HOME=/root/.cargo

echo "RUSTC=$(rustc --version)"
echo "CARGO=$(cargo --version)"
echo "FREE_KB=$(df /opt | tail -1 | awk '{print $4}')"

echo "MOUNT_TEST_START"
mount -t tmpfs -o size=8G tmpfs /tmp
echo "TMPFS_MOUNTED"
df -h /tmp

export CARGO_TARGET_DIR=/tmp/build/target
export CARGO_BUILD_JOBS=1
mkdir -p "$CARGO_TARGET_DIR"

cd /opt/starryos

# Prepend PROVIDE lines to ext_linker.ld if not already present.
# Uses sed instead of cat > to preserve all existing sections
# (including .tracepoint and .kallsyms that may be added upstream).
if ! grep -q 'PROVIDE(_ex_table_start' os/StarryOS/starryos/ext_linker.ld 2>/dev/null; then
    sed -i '1i PROVIDE(_ex_table_start = 0);\nPROVIDE(_ex_table_end = 0);\n' os/StarryOS/starryos/ext_linker.ld
fi
echo "LINKER_FIXED"

echo "CARGO_BUILD_START"
cargo build -p starryos --target riscv64gc-unknown-none-elf --features qemu,ax-driver/pci,ax-driver/virtio-blk,ax-driver/virtio-net,ax-driver/virtio-gpu,ax-driver/virtio-input,ax-driver/virtio-socket --offline 2>&1
echo "CARGO_BUILD_PASSED"

BINARY=/tmp/build/target/riscv64gc-unknown-none-elf/debug/starryos
if [ -f "$BINARY" ] && [ -s "$BINARY" ]; then
    echo "BINARY_EXISTS"
    ls -la "$BINARY"
    echo "SELFHOST_SUCCESS"
else
    echo "BINARY_MISSING"
    echo "SELFHOST_FAILED"
fi
