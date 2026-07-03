---
sidebar_position: 4
sidebar_label: "自定义平台"
---

# 自定义平台 `axplat-custom`

[platforms/axplat-custom](platforms/axplat-custom)（`publish = false`）是最小自定义平台模板，用来展示如何在不使用 `axplat-dyn` 的情况下实现 `ax-plat` 接口。它不是 workspace 成员，也不是可直接启动 QEMU 或真实硬件的完整平台；其中的内存、console、timer、IRQ 和 power 实现都是占位逻辑。

这个目录只作为可复制模板保留，根 `Cargo.toml` 不把它列入 `members` 或 `[workspace.dependencies]`。这样可以避免模板包进入正常构建、clippy、发布打包和 crates.io 依赖解析路径。

## crate 元数据

```toml
# platforms/axplat-custom/Cargo.toml
[features]
default = []
smp = ["ax-plat/smp"]
irq = ["ax-plat/irq"]

[dependencies]
ax-plat.workspace = true
rdrive.workspace = true

[package.metadata.axplat]
platform = "custom"
arch     = "aarch64"
crate    = "axplat_custom"
dynamic  = false
```

复制为真实平台并接入 workspace 后，`dynamic = false` 会让 `axbuild` 把它当作静态平台；依赖应保持最小，只引入 `ax-plat` 和实际需要的设备发现/驱动 glue。

## lib.rs 与文件结构

[platforms/axplat-custom/src/lib.rs](platforms/axplat-custom/src/lib.rs)：

```rust
#![no_std]
#[macro_use] extern crate ax_plat;

mod config;
mod console;
mod init;
mod mem;
mod power;
mod time;
#[cfg(feature = "irq")]
mod irq;

pub use mem::boot_stack_bounds;
pub use time::{enable_timer_irq, try_init_epoch_offset};
```

| 文件 | 职责 | 实现要点 |
| --- | --- | --- |
| [config.rs](platforms/axplat-custom/src/config.rs) | 平台常量 | `PLATFORM_NAME = "custom"`、`RAM_BASE = 0x8000_0000`、`RAM_SIZE = 128 MB`、`KERNEL_ASPACE_BASE = 0xffff_0000_0000_0000`；`PHYS_RAM_RANGES` / `RESERVED_RAM_RANGES` / `MMIO_RANGES` 静态数组 |
| [init.rs](platforms/axplat-custom/src/init.rs) | `InitIf` + `PlatformInfoIf` | `init_later` 中调用 `rdrive::init(rdrive::Platform::Static)` |
| [mem.rs](platforms/axplat-custom/src/mem.rs) | `MemIf` | 返回 config 中的常量 ranges；identity `phys_to_virt`/`virt_to_phys`；`boot_stack_bounds` 返回 `(0, 0)` |
| [console.rs](platforms/axplat-custom/src/console.rs) | `ConsoleIf` | `write_bytes` no-op；`device_id()` 返回 `Err(NotSpecified)`；IRQ 方法留空 |
| [time.rs](platforms/axplat-custom/src/time.rs) | `TimeIf` | 基于原子计数器的 mock 时间；`try_init_epoch_offset` 用于 RTC setter |
| [power.rs](platforms/axplat-custom/src/power.rs) | `PowerIf` | `system_off` / `system_reset` 用 `loop { spin_loop() }` 占位；`cpu_num() = 1` |
| [irq.rs](platforms/axplat-custom/src/irq.rs) | 可选 `IrqIf` | `handle(vector)` 把 `vector.0` 包成 legacy IRQ 后调用 `ax_plat::irq::dispatch_irq` |

每个文件都用 `#[impl_plat_interface] impl FooIf for FooImpl` 注册到 `ax-plat` 的单实现槽。

## 使用示例模板

使用自定义平台时需要同时做两件事：

1. 让 Cargo 把你的平台 crate 编进 `ax-hal` 的依赖图。
2. 让 `ax-hal` 的 build script 生成 `pub extern crate axplat_custom as selected;`。

`axplat-custom` 是 `publish = false` 的模板 crate，不能作为 `ax-hal` 的内置 optional dependency，也不能留在根 workspace dependency 表里。否则 `cargo package -p ax-hal` 会在 crates.io 上解析这个模板包并失败。真实项目应复制模板并在自己的私有 workspace / fork 中给 `ax-hal` 增加对应依赖，例如：

```toml
# os/arceos/modules/axhal/Cargo.toml
[features]
axplat-myplat = ["dep:axplat-myplat"]
smp = [
    "axplat-myplat?/smp",
    "axplat-dyn/smp",
    "ax-plat/smp",
]
irq = [
    "axplat-myplat?/irq",
    "ax-plat/irq",
    "axplat-dyn/irq",
    "dep:ax-kspin",
]

[dependencies]
axplat-myplat = { path = "../../../platforms/axplat-myplat", default-features = false, optional = true }
```

直接验证本地自定义平台：

```bash
AX_PLATFORM_CRATE=axplat_myplat \
cargo check -p ax-hal --features axplat-myplat
```

通过 `ax-feat` 组织 feature 的配置：

```toml
features = [
  "ax-feat/axplat-myplat",
]

[env]
AX_PLATFORM_CRATE = "axplat_myplat"
```

Rust std 应用可以在自己的 `ax-std`/`ax-feat` feature 转发中加入对应平台 feature 后使用：

```toml
features = [
  "ax-std/axplat-myplat",
]

[env]
AX_PLATFORM_CRATE = "axplat_myplat"
```

C app 或直接使用 `ax-libc` 的配置同理：在本地 `ax-libc` / `ax-feat` 中增加 feature 转发，再设置 `AX_PLATFORM_CRATE`。

## 改名使用

`axplat-custom` 是模板名，不是固定 ABI。用户复制模板并改名为 `myplat` 时，需要同步修改：

- 根 workspace `members` 和 `[workspace.dependencies]` 中新增真实平台的包名与路径。
- 新平台 `Cargo.toml` 的 `[package].name` 和 `[package.metadata.axplat].crate`。
- 本地 `ax-hal` 中的可选依赖和 feature。
- 本地 `ax-feat`、`ax-std`、`ax-libc` 中对应的 feature 转发（如果通过这些上层 crate 选择平台）。
- Build Info 中的 feature 名，以及 `[env] AX_PLATFORM_CRATE`。

例如包名为 `axplat-myplat` 时，Rust crate 标识符通常是 `axplat_myplat`：

```toml
features = [
  "ax-std/axplat-myplat",
]

[env]
AX_PLATFORM_CRATE = "axplat_myplat"
```

## 必须替换的板级事实

`axplat-custom` 的所有方法都是占位实现，真实部署时必须替换：

- **内存**：RAM、reserved RAM、MMIO 范围（[config.rs](platforms/axplat-custom/src/config.rs)）。
- **地址转换**：`phys_to_virt()`、`virt_to_phys()` 和 `kernel_aspace()`（[mem.rs](platforms/axplat-custom/src/mem.rs)）。
- **启动**：启动入口、linker script、boot stack 和 `.bss` 清零时机。模板的 `boot_stack_bounds` 返回 `(0, 0)`，真实平台必须返回非零。
- **console**：早期 console 读写，以及运行时 console ownership（[console.rs](platforms/axplat-custom/src/console.rs)）。
- **时间**：tick 读取、tick/nanos 换算、timer IRQ 和 one-shot timer（[time.rs](platforms/axplat-custom/src/time.rs)）。
- **IRQ**：控制器、ACK/EOI、source resolver、IPI 和 affinity（[irq.rs](platforms/axplat-custom/src/irq.rs)）。
- **电源**：`system_off()`、`system_reset()` 和 SMP `cpu_boot()`（[power.rs](platforms/axplat-custom/src/power.rs)）。

模板的 `rdrive::init(rdrive::Platform::Static)` 只是占位；要在没有 FDT/ACPI 的板子上枚举设备，需要按 [devices.md](devices.md) 描述注册 `DriverRegister`。

## QEMU 状态

`axplat-custom` 当前只是接口模板，不是完整 QEMU 平台。用它替换 `axplat-dyn` 运行 `arceos-helloworld` 的 aarch64 QEMU 配置时，构建可以进入最终链接阶段，但会因为 `axplat-dyn` 与 `axplat-custom` 同时实现 `ax-plat` crate-interface 符号而链接失败。

要让自定义平台真正启动 QEMU，需要保证最终镜像只链接一个 `ax-plat` 实现 crate，并补齐真实 QEMU 启动入口、console、timer、内存布局和 IRQ 实现。可参考 [dynamic.md](dynamic.md) 中 `axplat-dyn` 的对应实现，或借鉴 [somehal.md](somehal.md) 中的架构后端。
