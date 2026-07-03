---
sidebar_position: 7
sidebar_label: "自定义平台"
---

# 自定义平台

默认构建路径仍使用 `axplat-dyn`。当需要验证一个不依赖 `axplat-dyn` 的外部板级平台时，可以提供一个独立的 `ax-plat` 实现 crate，并通过对应 feature 和 `AX_PLATFORM_CRATE` 让 `ax-hal` 在构建时选中它。

仓库内提供 `platforms/axplat-custom` 作为最小模板。它实现了 `InitIf`、`PlatformInfoIf`、`MemIf`、`TimeIf`、`ConsoleIf`、`PowerIf`，并在 `init_later()` 中初始化 `rdrive::Platform::Static`，方便没有 FDT/ACPI/PCI 描述的板级 glue 使用 `ProbeKind::Static` 显式注册设备。

## 切换方式

使用示例平台时需要同时做两件事：

1. 让 Cargo 把 `axplat-custom` 编进依赖图。
2. 让 `ax-hal` 的 build script 生成 `pub extern crate axplat_custom as selected;`。

对直接 Cargo 构建，可使用：

```bash
AX_PLATFORM_CRATE=axplat_custom \
cargo build -p ax-hal --features axplat-custom
```

对通过 `ax-feat` 组织 feature 的 ArceOS/StarryOS 应用，在 Build Info 中加入 feature 和环境变量：

```toml
features = [
  "ax-feat/axplat-custom",
]

[env]
AX_PLATFORM_CRATE = "axplat_custom"
```

如果应用经 `ax-std` 组织 feature，则使用 `ax-std/axplat-custom`：

```toml
features = [
  "ax-std/axplat-custom",
]

[env]
AX_PLATFORM_CRATE = "axplat_custom"
```

C app 或直接使用 `ax-libc` 的配置可使用 `ax-libc/axplat-custom`。底层 `ax-hal/axplat-custom` 也存在，但普通应用应优先通过自己的上层 feature 前缀转发，避免 Cargo 拒绝非当前 package 的 feature。

未设置 `AX_PLATFORM_CRATE` 时，`ax-hal` 默认选择 `axplat_dyn`。

## 改名使用

`axplat-custom` 是模板名，不是固定 ABI。用户复制模板并改名为 `myplat` 时，需要同步修改：

- workspace `members` 和 `[workspace.dependencies]` 中的 `axplat-custom` 包名、路径。
- `platforms/myplat/Cargo.toml` 的 `[package].name` 和 `package.metadata.axplat.crate`。
- `ax-hal` 中的可选依赖和 feature，例如把 `axplat-custom` 改成 `myplat`。
- `ax-feat`、`ax-std`、`ax-libc` 中对应的 feature 转发。
- Build Info 中的 feature 名，以及 `[env] AX_PLATFORM_CRATE`。例如 crate 标识符为 `myplat` 时设置 `AX_PLATFORM_CRATE = "myplat"`；包名是 `axplat-myplat` 时通常是 `AX_PLATFORM_CRATE = "axplat_myplat"`。

## QEMU 启动状态

`axplat-custom` 当前是接口模板，不是可直接启动 QEMU 的完整平台。用它替换 `axplat-dyn` 运行 `arceos-helloworld` 的 aarch64 QEMU 配置时，构建可以进入最终链接阶段，但会因为 `axplat-dyn` 与 `axplat-custom` 同时实现 `ax-plat` crate-interface 符号而链接失败。

要让自定义平台真正启动 QEMU，需要继续完成两件事：

- 平台选择必须保证最终镜像只链接一个 `ax-plat` 实现 crate。
- 自定义平台需要提供真实 QEMU 启动入口、console、timer、内存布局和 IRQ 实现，而不是当前模板里的占位实现。

## 平台 crate 结构

自定义平台 crate 应至少包含：

| 模块职责 | 需要实现的接口 |
| --- | --- |
| 生命周期 | `ax_plat::init::InitIf` |
| 平台标识 | `ax_plat::platform::PlatformInfoIf` |
| 内存布局 | `ax_plat::mem::MemIf` |
| 时间源 | `ax_plat::time::TimeIf` |
| 早期控制台 | `ax_plat::console::ConsoleIf` |
| 电源与 CPU 数 | `ax_plat::power::PowerIf` |
| 中断，可选 | `ax_plat::irq::IrqIf` |

`platforms/axplat-custom/src/` 已按职责拆分为 `config.rs`、`init.rs`、`mem.rs`、`time.rs`、`console.rs`、`power.rs` 和可选的 `irq.rs`。其中的地址、timer、console 和 IRQ 都是占位实现。移植到真实平台时，需要替换：

- `PHYS_RAM_RANGES`、`RESERVED_RAM_RANGES`、`MMIO_RANGES`
- `phys_to_virt()` / `virt_to_phys()` / `kernel_aspace()`
- console 读写函数
- tick 读取、tick/nanos 换算和 one-shot timer
- IRQ enable、ack/EOI、source resolver 和 IPI
- `system_off()` / `system_reset()` / `cpu_boot()`

## 设备注册

有固件描述的平台应优先使用 FDT、ACPI 或 PCI probe。没有固件描述时，可在平台初始化后注册静态 probe：

```rust
rdrive::register_add(rdrive::register::DriverRegister {
    name: "custom-uart",
    level: rdrive::register::ProbeLevel::PostKernel,
    priority: rdrive::register::ProbePriority::DEFAULT,
    probe_kinds: &[rdrive::register::ProbeKind::Static {
        on_probe: probe_uart,
    }],
});
```

`ProbeKind::Static` 只是驱动发现来源，不是旧的 `myplat` / `defplat` Cargo feature 平台选择机制。
