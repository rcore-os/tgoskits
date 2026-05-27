# StarryOS K230 QEMU Cases

This group contains K230-specific StarryOS QEMU cases. It is intentionally
separate from the default `normal` group because it requires a QEMU build that
contains the K230 machine and KPU model.

Expected local layout for running these cases:

```text
target/qemu-k230/bin/qemu-system-riscv64
target/qemu-k230/pc-bios/
```

The binary should come from the K230 QEMU branch, and `pc-bios` should point to
the matching QEMU source tree's `pc-bios` directory. One local setup option is:

```sh
mkdir -p target/qemu-k230/bin
ln -sf /mnt/tmp/tgoskits/target/qemu-k230-docker-build/qemu-system-riscv64 \
  target/qemu-k230/bin/qemu-system-riscv64
ln -sfn /mnt/tmp/qemu/pc-bios target/qemu-k230/pc-bios
```

Then run the group with the K230 QEMU binary prepended to `PATH`:

```sh
PATH="$PWD/target/qemu-k230/bin:$PATH" \
cargo xtask starry test qemu --test-group k230-qemu --arch riscv64 -c kpu-smoke
```
