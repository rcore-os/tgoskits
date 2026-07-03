---
sidebar_position: 4
sidebar_label: "自定义平台"
---

# 自定义平台 `axplat-custom`

[platforms/axplat-custom](platforms/axplat-custom)（`publish = false`）是最小自定义平台模板，用来展示如何在不使用 `axplat-dyn` 的情况下实现 `ax-plat` 接口。它不是可直接启动 QEMU 或真实硬件的完整平台；其中的内存、console、timer、IRQ 和 power 实现都是占位逻辑。

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

`dynamic = false` 让 `axbuild` 把它当作静态模板，依赖保持最小：只引入 `ax-plat` 和 `rdrive`。

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

使用示例平台时需要同时做两件事：

1. 让 Cargo 把 `axplat-custom` 编进依赖图。
2. 让 `ax-hal` 的 build script 生成 `pub extern crate axplat_custom as selected;`。

直接验证 `ax-hal`：

```bash
AX_PLATFORM_CRATE=axplat_custom \
cargo check -p ax-hal --features axplat-custom
```

通过 `ax-feat` 组织 feature 的配置：

```toml
features = [
  "ax-feat/axplat-custom",
]

[env]
AX_PLATFORM_CRATE = "axplat_custom"
```

Rust std 应用使用 `ax-std/axplat-custom`：

```toml
features = [
  "ax-std/axplat-custom",
]

[env]
AX_PLATFORM_CRATE = "axplat_custom"
```

C app 或直接使用 `ax-libc` 的配置可使用 `ax-libc/axplat-custom`。底层 `ax-hal/axplat-custom` 也存在，但普通应用应优先通过自己的上层 feature 前缀转发。

## 改名使用

`axplat-custom` 是模板名，不是固定 ABI。用户复制模板并改名为 `myplat` 时，需要同步修改：

- workspace `members` 和 `[workspace.dependencies]` 中的包名与路径。
- 新平台 `Cargo.toml` 的 `[package].name` 和 `[package.metadata.axplat].crate`。
- `ax-hal` 中的可选依赖和 feature。
- `ax-feat`、`ax-std`、`ax-libc` 中对应的 feature 转发。
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
