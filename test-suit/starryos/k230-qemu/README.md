# StarryOS K230 QEMU Test Group

This group contains StarryOS QEMU tests for the K230 machine.

The first stage validates the board bring-up path only:

- dynamic RISC-V platform boot with the K230 DTB;
- K230 SDHCI rootfs wiring through `-drive if=sd,...`;
- a minimal user-space shell command from the mounted rootfs.

KPU device and NNCase runtime tests are intentionally added in later PRs.

Local validation requires a QEMU build with the K230 machine model. The expected
layout is `target/qemu-k230-docker-build` relative to the repository root; put
that directory before the default QEMU path when running this group.
