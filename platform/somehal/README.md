# SomeHAL - 平台硬件抽象层实现

SomeHAL (Sparreal Hardware Abstraction Layer) 是 Sparreal OS 的平台硬件抽象层实现，提供统一的跨平台硬件接口，位于内核核心 (`sparreal-kernel`) 和底层架构支持 (`someboot`) 之间。

## 📋 目录

- [概述](#概述)
- [架构支持](#架构支持)
- [主要特性](#主要特性)
- [目录结构](#目录结构)
- [快速开始](#快速开始)
- [平台接口](#平台接口)
- [特性标志](#特性标志)
- [依赖说明](#依赖说明)
- [开发指南](#开发指南)

## 概述

SomeHAL 负责将通用的内核接口与具体的硬件平台连接起来，提供：

- **统一平台接口**: 通过 `PlatOp` trait 定义平台操作抽象
- **多架构支持**: AArch64 (ARMv8) 和 LoongArch64
- **中断管理**: GICv2/GICv3 支持，运行时版本检测
- **驱动集成**: 通过 `rdrive` 驱动框架管理硬件设备
- **内存抽象**: 统一的 I/O 内存重映射接口

### 设计原则

1. **分层设计**: `sparreal-rt` → `somehal` → `someboot`
2. **平台无关**: 内核核心不需要知道底层硬件细节
3. **驱动优先**: 硬件初始化通过驱动系统自动完成
4. **类型安全**: 充分利用 Rust 的类型系统确保安全性

## 架构支持

### AArch64 (ARMv8)

**支持的中断控制器**:
- ✅ GICv2 (ARM Cortex-A15 GIC, GIC-400)
- ✅ GICv3 (现代 ARMv8 处理器)
- 🔧 运行时版本自动检测

**定时器支持**:
- ARM Generic Timer (EL1/EL2)

**虚拟化扩展**:
- GICv2: GICH/GICV 虚拟接口支持
- GICv3: 完整的虚拟化支持

### LoongArch64

- 基础架构支持
- 平台特定的中断控制器

## 主要特性

### 🔄 多版本 GIC 支持

```rust
// 运行时自动检测 GIC 版本
pub fn init_current_cpu() {
    if v3::is_v3() {
        v3::init_cpu();
    } else {
        v2::init_cpu();
    }
}
```

### 🎯 驱动系统集成就绪

- 通过设备树 (FDT) 自动探测硬件
- 支持动态设备注册
- 统一的探测优先级管理

### 🔌 中断处理统一接口

```rust
// 上层统一接口，底层版本无关
fn __aarch64_irq_handler() {
    if v3::is_v3() {
        v3::handle_irq();
    } else {
        v2::handle_irq();
    }
}
```

## 目录结构

```
somehal/
├── src/
│   ├── arch/                 # 架构特定实现
│   │   ├── aarch64/         # AArch64 架构
│   │   │   ├── gic/         # 通用中断控制器
│   │   │   │   ├── mod.rs   # GIC 统一接口
│   │   │   │   ├── v2.rs    # GICv2 实现
│   │   │   │   └── v3.rs    # GICv3 实现
│   │   │   ├── systick.rs   # 系统定时器
│   │   │   └── mod.rs       # AArch64 平台实现
│   │   └── loongarch64/     # LoongArch64 架构
│   ├── common.rs            # 平台通用定义
│   ├── driver.rs            # 驱动管理
│   ├── irq.rs               # 中断接口
│   ├── lib.rs               # 库入口
│   └── setup.rs             # 初始化设置
├── Cargo.toml               # 项目配置
└── README.md                # 本文档
```

## 快速开始

### 使用 Entry 宏

SomeHAL 提供了 `#[somehal::entry]` 宏来自动化初始化流程：

```rust
use somehal::{KernelOp, PagingResult};
use sparreal_kernel::os::mem;

// 1. 定义内核类型并实现 KernelOp trait
pub struct Kernel;

impl KernelOp for Kernel {
    fn ioremap(&self, paddr: usize, size: usize) -> PagingResult<*mut u8> {
        // 提供内存重映射接口
        mem::ioremap(paddr.into(), size)
            .map(|addr| addr.raw() as *mut u8)
    }
}

// 2. 使用 entry 宏自动初始化
#[somehal::entry(Kernel)]
fn main() -> ! {
    // somehal::init(&Kernel) 已由宏自动调用

    // ... 内存初始化 ...

    // 3. 分页后启动驱动系统
    somehal::post_paging();

    // ... 继续启动 ...
}
```

**宏的参数要求**：
- **必需参数**: 必须指定实现 `KernelOp` trait 的类型（如 `Kernel`）
- **自动行为**: 宏会在函数开头自动插入 `somehal::init(&<type>)` 调用
- **错误提示**: 无参数时会显示清晰的编译错误

### KernelOp Trait

所有使用 SomeHAL 的平台必须实现 `KernelOp` trait：

```rust
use somehal::KernelOp;

pub trait KernelOp {
    /// 内存重映射接口
    ///
    /// 将物理地址映射到内核虚拟地址空间
    ///
    /// # 参数
    /// - `paddr`: 物理地址
    /// - `size`: 映射大小（字节）
    ///
    /// # 返回
    /// 映射后的虚拟地址指针
    fn ioremap(&self, paddr: usize, size: usize) -> PagingResult<*mut u8>;
}
```

### 初始化流程

1. **自动初始化** (`#[somehal::entry(Kernel)]`)
   - 宏自动调用 `somehal::init(&Kernel)`
   - 设置内核操作回调接口

2. **驱动系统启动** (`somehal::post_paging()`)
   - 在分页系统初始化后调用
   - 通过设备树自动探测硬件
   - 初始化中断控制器和定时器

### 中断处理

```rust
use somehal::irq::*;

// 使能中断
irq_set_enable(irq_id, true);

// 禁用中断
irq_set_enable(irq_id, false);

// 获取系统定时器 IRQ
let timer_irq = systick_irq();
```

### 平台操作

SomeHAL 通过 `KernelOp` trait 提供平台特定功能：

```rust
use somehal::KernelOp;

// 在平台运行时中实现
pub struct Kernel;

impl KernelOp for Kernel {
    fn ioremap(&self, paddr: usize, size: usize) -> PagingResult<*mut u8> {
        // 调用内核的内存管理接口
        sparreal_kernel::os::mem::ioremap(paddr.into(), size)
            .map(|addr| addr.raw() as *mut u8)
    }
}
```

## 平台接口

### 核心接口

SomeHAL 提供两个核心接口：

#### 1. `KernelOp` Trait

内核操作接口，由平台运行时实现：

```rust
pub trait KernelOp {
    /// 内存重映射接口
    fn ioremap(&self, paddr: usize, size: usize) -> PagingResult<*mut u8>;
}
```

#### 2. `PlatOp` Trait

平台操作接口，由架构实现提供：

```rust
pub trait PlatOp {
    /// 设置 IRQ 使能状态
    fn irq_set_enable(irq: IrqId, enable: bool);

    /// 获取系统定时器 IRQ ID
    fn systick_irq() -> IrqId;
}
```

### 初始化函数

| 函数 | 调用时机 | 说明 |
|------|---------|------|
| `init(kernel)` | entry 宏自动调用 | 设置内核操作回调 |
| `post_paging()` | 分页系统初始化后 | 启动驱动系统，初始化硬件设备 |

### I/O 内存映射

通过 `KernelOp::ioremap` 进行内存映射：

```rust
// 在内核代码中
let virt_addr = somehal::ioremap(0x0800_0000, 0x1000)?;
```

## 特性标志

| 特性 | 描述 | 默认 |
|------|------|------|
| `efi` | UEFI 固件支持 | No |
| `hv` | 虚拟化支持 (Hypervisor) | No |
| `mmu` | MMU/分页支持 | No |
| `uspace` | 用户空间支持 | No |

### 使用示例

```toml
[dependencies]
somehal = { version = "0.5", features = ["hv", "uspace"] }
```

## 依赖说明

### 核心依赖

- **`someboot`**: 底层架构抽象和引导支持
- **`rdrive`**: 统一驱动框架
- **`rdif-intc`**: 中断控制器驱动接口
- **`page-table-generic`**: 通用页表管理
- **`kernutil`**: 内核工具库（StaticCell 等）

### AArch64 特定依赖

- **`aarch64-cpu`**: AArch64 寄存器访问
- **`arm-gic-driver`**: ARM GIC 驱动实现
- **`tock-registers`**: 寄存器操作接口

### 构建依赖

- **`somehal-macros`**: 平台宏（entry、irq_handler 等）

## 开发指南

### 创建新平台

使用 SomeHAL 创建新平台运行时的步骤：

#### 1. 定义内核类型

```rust
// platform/my-platform/src/lib.rs
use somehal::KernelOp;

pub struct Kernel;

impl KernelOp for Kernel {
    fn ioremap(&self, paddr: usize, size: usize) -> PagingResult<*mut u8> {
        // 实现内存映射逻辑
        // 通常调用 sparreal_kernel::os::mem::ioremap
    }
}
```

#### 2. 使用 entry 宏

```rust
#[somehal::entry(Kernel)]
fn main() -> ! {
    // somehal::init(&Kernel) 已自动调用

    // 初始化内存管理
    sparreal_kernel::os::mem::paging::init();

    // 启动驱动系统
    somehal::post_paging();

    // 运行内核
    sparreal_kernel::run_kernel()
}
```

#### 3. 实现平台特定接口

在 `hal_impl.rs` 中实现内核的 HAL trait：

```rust
use sparreal_kernel::hal::al;

pub struct PlatformImpl;

#[sparreal_macros::api_impl]
impl Platform for PlatformImpl {
    unsafe fn wait_for_interrupt() {
        // 平台特定的 WFI 实现
    }

    fn shutdown() -> ! {
        // 平台特定的关机实现
        loop {}
    }
}
```

### 添加新架构支持

1. 在 `src/arch/` 下创建新目录
2. 实现 `PlatOp` trait
3. 提供中断控制器支持（如果需要）
4. 实现定时器接口
5. 更新 `src/lib.rs` 添加条件编译路径

### 添加新的中断控制器

参考 GICv2/v3 实现模式：

1. 创建独立的模块文件
2. 实现统一的接口函数：
   - `init_cpu()`: CPU 接口初始化
   - `handle_irq()`: 中断处理
   - `irq_set_enable()`: IRQ 使能控制
3. 在上层添加版本检测逻辑
4. 使用 `StaticCell` 管理全局状态

### 宏使用最佳实践

**entry 宏** (`#[somehal::entry]`)：
- ✅ **必须提供参数**: 指定实现 `KernelOp` 的类型
- ✅ **每个项目仅一次**: 整个依赖图只能有一个入口点
- ✅ **函数签名**: `fn main() -> !` 或 `unsafe fn main() -> !`
- ❌ **不要手动调用**: `somehal::init()` 由宏自动处理

**irq_handler 宏** (`#[somehal::irq_handler]`)：
```rust
#[somehal::irq_handler]
fn my_irq_handler(irq: someboot::irq::IrqId) {
    // 处理中断
    sparreal_kernel::os::irq::handle_irq(irq);
}
```

### 代码风格

- 使用 `#![no_std]` 环境
- 遵循 Rust 2024 Edition 规范
- 使用 `log` crate 进行日志输出
- 错误处理使用 `thiserror` 和 `anyhow`

## 相关文档

- [Sparreal OS CLAUDE.md](../../CLAUDE.md) - 项目总览
- [someboot 文档](../../crates/someboot/README.md) - 底层架构抽象
- [sparreal-kernel 文档](../../crates/sparreal-kernel/CLAUDE.md) - 内核核心

## 许可证

本项目采用与 Sparreal OS 相同的开源许可证。

## 贡献

欢迎提交 Issue 和 Pull Request！

在提交代码前，请确保：

1. 代码通过 `cargo fmt` 格式化
2. 通过 `cargo clippy` 检查
3. 在目标平台上测试通过
4. 更新相关文档

---

**最后更新**: 2026-01-21
