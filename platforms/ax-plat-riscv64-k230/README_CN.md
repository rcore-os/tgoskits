# ax-plat-riscv64-k230

`ax-plat-riscv64-k230` 是面向 QEMU Kendryte K230 机器的 `axplat` 平台适配。

该平台使用 Kunos QEMU 流程中的 K230 direct-boot 布局：内核加载到
`0x0820_0000`，UART0 位于 `0x9140_0000`，PLIC 位于 `0xf000_00000`，KPU
CFG/L2 窗口分别位于 `0x8040_0000` 和 `0x8000_0000`。
