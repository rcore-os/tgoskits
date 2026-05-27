# ax-plat-riscv64-k230

`ax-plat-riscv64-k230` provides the `axplat` hardware abstraction layer for
the QEMU Kendryte K230 machine.

The platform follows the K230 direct-boot layout used by the Kunos QEMU flow:
kernel at `0x0820_0000`, UART0 at `0x9140_0000`, PLIC at `0xf000_00000`, and
KPU CFG/L2 windows at `0x8040_0000` and `0x8000_0000`.
