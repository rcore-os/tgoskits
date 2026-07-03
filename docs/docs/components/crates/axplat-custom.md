# `axplat-custom`

> 路径：`platforms/axplat-custom`
> 类型：库 crate
> 分层：平台层 / 自定义平台示例

`axplat-custom` 是一个最小自定义平台模板，用来展示如何在不使用 `axplat-dyn` 的情况下实现 `ax-plat` 接口。它不是可直接运行在真实硬件上的平台包；其中的内存、console、timer、IRQ 和 power 实现都是占位逻辑，并通过 `publish = false` 禁止发布到 crates.io。

## 作用

- 作为独立平台 crate 实现 `ax_plat::{InitIf, PlatformInfoIf, MemIf, TimeIf, ConsoleIf, PowerIf}`。
- 可选实现 `smp` 和 `irq` feature 下的接口，保证模板能随 `ax-hal` feature 编译。
- 在 `init_later()` 中调用 `rdrive::init(rdrive::Platform::Static)`，展示无固件描述平台的静态 probe 入口。
- 配合 `AX_PLATFORM_CRATE=axplat_custom` 和 `ax-hal/axplat-custom` 选择该平台。

## 代码结构

- `config.rs`：平台名称、RAM/reserved RAM/MMIO 范围和 kernel address space。
- `init.rs`：早期/后期初始化，以及静态 `rdrive` probe 入口。
- `mem.rs`：物理内存布局和地址转换。
- `time.rs`：tick、时间换算、epoch offset 和 timer IRQ helper。
- `console.rs`：早期 console 输入输出。
- `power.rs`：关机、重启、CPU 数量和可选 SMP 启动。
- `irq.rs`：可选中断控制接口。

## 替换点

真实平台移植时应替换模板中的板级事实：

- RAM、reserved RAM、MMIO 范围
- 物理/虚拟地址转换和 kernel address space
- 早期 console 读写
- 单调时钟、timer IRQ 和 one-shot timer
- IRQ controller、IPI、affinity 和 firmware source resolver
- 电源管理和 SMP bring-up

## 使用

详见 [自定义平台](/docs/build/custom-platform)。
