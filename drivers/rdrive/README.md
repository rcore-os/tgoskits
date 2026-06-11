# rdrive

[![Crates.io](https://img.shields.io/crates/v/rdrive)](https://crates.io/crates/rdrive)
[![License](https://img.shields.io/crates/l/rdrive)](LICENSE)

`rdrive` 是一个用于裸机操作系统（Bare-metal OS）的动态驱动管理框架，提供设备探测、驱动注册、设备管理等核心功能。

## 功能特性

- **设备树（FDT）支持**：自动从 Flattened Device Tree 解析设备信息并匹配驱动
- **PCIe 设备支持**：支持 PCI Express 设备的探测和管理
- **动态驱动注册**：支持编译时和运行时驱动注册
- **多级探测机制**：支持内核前（Pre-kernel）和内核后（Post-kernel）两阶段设备探测
- **优先级管理**：驱动按优先级排序，确保依赖设备先初始化
- **线程安全**：使用自旋锁保证多核环境下的安全访问
- **`no_std` 兼容**：适用于裸机环境，不依赖标准库

## 架构概述

```
┌─────────────────────────────────────────────────────────┐
│                    应用层 (Applications)                  │
├─────────────────────────────────────────────────────────┤
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────┐  │
│  │   Manager   │  │   Probe     │  │    Register     │  │
│  │  (设备管理器) │  │  (设备探测)  │  │   (驱动注册)    │  │
│  └─────────────┘  └─────────────┘  └─────────────────┘  │
├─────────────────────────────────────────────────────────┤
│                    平台抽象层 (Platform)                  │
│         ┌──────────┐          ┌──────────┐             │
│         │   FDT    │          │   PCI    │             │
│         └──────────┘          └──────────┘             │
├─────────────────────────────────────────────────────────┤
│                    驱动接口层 (rdif-*)                    │
│    rdif-base │ rdif-intc │ rdif-clk │ rdif-pcie ...     │
└─────────────────────────────────────────────────────────┘
```

## 快速开始

### 1. 添加依赖

在 `Cargo.toml` 中添加：

```toml
[dependencies]
rdrive = "0.18"
```

### 2. 初始化 rdrive

```rust
use core::ptr::NonNull;
use rdrive::{init, Platform};

// 从 FDT 地址初始化
let fdt_addr: NonNull<u8> = /* 设备树地址 */;
init(Platform::Fdt { addr: fdt_addr }).expect("rdrive init failed");
```

### 3. 注册驱动

```rust
use rdrive::{
    register_add,
    probe::OnProbeError,
    register::{ProbeFdt, ProbeKind, ProbeLevel, ProbePriority},
};

fn my_probe_function(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, plat_dev) = probe.into_parts();
    // 初始化设备并通过 plat_dev.register(...) 注册
    Ok(())
}

// 定义 FDT 驱动注册信息
static MY_DRIVER_REGISTER: DriverRegister = DriverRegister {
    name: "my-driver",
    level: ProbeLevel::PreKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["vendor,my-device"],
        on_probe: my_probe_function,
    }],
};

// 注册驱动
register_add(MY_DRIVER_REGISTER);
```

### 4. 执行设备探测

```rust
use rdrive::probe_pre_kernel;

// 在内核初始化前探测设备
probe_pre_kernel().expect("probe failed");
```

### 5. 获取设备实例

```rust
use rdrive::get_device;
use rdif_intc::Intc;

// 获取中断控制器设备
let intc: Device<Intc> = get_device(irq_id).expect("device not found");
```

## 核心模块

### Manager（设备管理器）

`Manager` 是 rdrive 的核心，负责：
- 管理所有注册的驱动
- 存储已探测到的设备实例
- 提供设备查找接口

### Probe（设备探测）

支持多种设备探测方式：
- **FDT 探测**：根据设备树 compatible 字符串匹配驱动
- **PCI 探测**：扫描 PCI 总线发现设备

探测级别：
- `PreKernel`：内核初始化前探测（如中断控制器、时钟）
- `PostKernel`：内核初始化后探测（如存储、网络设备）

### Register（驱动注册）

驱动注册支持：
- 编译时静态注册（通过链接器脚本）
- 运行时动态注册
- 优先级排序（数值越小优先级越高）

## 预定义优先级

```rust
pub const CLK: ProbePriority = ProbePriority(6);    // 时钟
pub const INTC: ProbePriority = ProbePriority(10);  // 中断控制器
pub const DEFAULT: ProbePriority = ProbePriority(256); // 默认
```

## 驱动接口（rdif）

rdrive 通过 `rdif-*` 系列 crate 定义标准驱动接口：

| 接口 | 描述 |
|------|------|
| `rdif-base` | 基础驱动接口和错误类型 |
| `rdif-intc` | 中断控制器接口 |
| `rdif-clk` | 时钟驱动接口 |
| `rdif-pcie` | PCIe 接口 |
| `rdif-serial` | 串口驱动接口 |
| `rdif-block` | 块设备接口 |
| `rdif-timer` | 定时器接口 |

## 平台支持

- **架构**：AArch64、LoongArch64、RISC-V、x86_64
- **引导方式**：FDT (Flattened Device Tree)
- **总线**：PCI Express

## 许可证

本项目采用 MIT 许可证，详见 [LICENSE](../../LICENSE) 文件。

## 相关项目

- [sparreal-os](https://github.com/drivercraft/sparreal-os) - 基于 rdrive 的 Rust 实时操作系统
- [rdif](https://github.com/drivercraft/rdrive/tree/main/driver/interface) - 驱动接口定义

## 贡献

欢迎提交 Issue 和 PR！请确保代码通过 `cargo check` 和 `cargo clippy` 检查。

---

由 [周睿](mailto:zrufo747@outlook.com) 创建并维护
