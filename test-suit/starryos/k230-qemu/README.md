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
ln -sf /Users/joshua/tmp/tgoskits/target/qemu-k230-docker-build/qemu-system-riscv64 \
  target/qemu-k230/bin/qemu-system-riscv64
ln -sfn /Users/joshua/tmp/qemu/pc-bios target/qemu-k230/pc-bios
```

Then run the group with the K230 QEMU binary prepended to `PATH`:

```sh
PATH="$PWD/target/qemu-k230/bin:$PATH" \
cargo xtask starry test qemu --test-group k230-qemu --arch riscv64 -c kpu-smoke
```

When using the project Docker image with the locally built K230 QEMU binary,
make sure the container has `libfdt.so.1` available. The command used during
this phase installed `libfdt1` before running the case.

For the local symlinks above, mount the K230 QEMU build and `pc-bios` into the
container at the same absolute paths:

```sh
docker run --rm \
  -v "$PWD":/mnt \
  -v /Users/joshua/tmp/tgoskits/target/qemu-k230-docker-build:/qemu-k230-build \
  -v /Users/joshua/tmp/qemu/pc-bios:/qemu-pc-bios \
  -w /mnt \
  starryos-dev:ubuntu-qemu10.2.1 \
  bash -lc 'ldconfig -p | grep -q libfdt.so.1 || (apt-get update && apt-get install -y libfdt1); \
    PATH="$PWD/target/qemu-k230/bin:/opt/riscv64-linux-musl-cross/bin:$PATH" \
    cargo xtask starry test qemu --test-group k230-qemu --arch riscv64 -c kpu-smoke'
```

The guest smoke program includes the shared KPU userspace ABI header from:

```text
drivers/npu/k230-kpu/include/k230_kpu_uapi.h
```

Keep new KPU userspace tests on that header instead of copying ioctl or mmap
constants into each case.

The smoke output should include the FDT-probed KPU resources:

```text
KPU_SMOKE: info cfg=0x80400000+0x800 l2=0x80000000+0x200000 irq=189 flags=0x3
KPU_SMOKE: run_wait_done status=0x0000000400000004 irq_count=0->1
KPU_SMOKE_PASS
```
