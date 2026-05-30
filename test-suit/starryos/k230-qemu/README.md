# StarryOS K230 QEMU Test Group

This group contains StarryOS QEMU tests for the K230 machine.

The `boot` case validates the board bring-up path:

- dynamic RISC-V platform boot with the K230 DTB;
- K230 SDHCI rootfs wiring through `-drive if=sd,...`;
- a minimal user-space shell command from the mounted rootfs.

The `kpu-smoke` case validates the first KPU userspace interface:

- `/dev/kpu` and `/dev/kpu0` are registered from the FDT KPU node;
- KPU CFG/L2/fake-output/runtime windows can be accessed through restricted
  `mmap`;
- KPU ioctl submit and wait paths observe QEMU done status and IRQ progress;
- small runtime direct-I/O commands produce checkable output bytes.

Local validation requires a QEMU build with the K230 machine model. The expected
layout is `target/qemu-k230-docker-build` relative to the repository root; put
that directory before the default QEMU path when running this group.

Example:

```sh
cargo xtask starry test qemu --test-group k230-qemu --arch riscv64 -c kpu-smoke
```

The smoke output should include:

```text
KPU_SMOKE: opened /dev/kpu
KPU_SMOKE: info cfg=0x80400000+0x800 l2=0x80000000+0x200000 irq=189 flags=0xf
KPU_SMOKE: fake_output_zeroed
KPU_SMOKE: runtime_image file_runtime_arg_table_direct_io
KPU_SMOKE_PASS
```

NNCase runtime and real `.kmodel` demos are intentionally left to a later PR.
