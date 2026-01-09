# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 项目概述

Sparreal OS 是一个用 Rust 开发的嵌入式实时操作系统内核，支持多架构（AArch64 和 LoongArch64），专注于裸机开发和 `no_std` 环境。

### 核心特性

- **多架构支持**: AArch64 (ARMv8)、LoongArch64
- **异步执行器**: 基于 embassy 设计的单 CPU 异步任务调度器
- **双堆内存管理**: 32 位/64 位分离堆、伙伴系统分配器
- **硬件抽象层**: 统一的跨平台 HAL 接口
- **页表管理**: 通用页表抽象，支持多级页表和大页映射

## 构建和测试

### 环境要求

- Rust nightly toolchain (见 [rust-toolchain.toml](rust-toolchain.toml))
- [ostool](https://github.com/qclic/ostool) 构建工具
- QEMU 模拟器（用于测试）
- GDB multiarch（用于调试，Windows 上通过 MSYS2 安装）

### 安装依赖

```bash
cargo install ostool
```

### 必须遵循

修改完代码后，确保可以编译通过，编译命令

```bash

# aarch64
ostool build -c ./build-config/aarch64.toml

# loongarch64
ostool build -c ./build-config/loongarch64.toml
```

需要执行 `cargo fmt --all` 格式化代码。

### 测试特定应用/套件

项目使用配置文件系统，每个应用或测试套件都有自己的 `.toml` 配置：

```bash
# 测试 AArch64 timer 套件
ostool run -c ./test-suit/timer/aarch64.toml qemu -q ./test-suit/timer/qemu-aarch64.toml

# 测试 LoongArch64 async 套件
ostool run -c ./test-suit/async/loongarch64.toml qemu -q ./test-suit/async/qemu-la64.toml

# 测试 helloworld 应用
ostool run -c ./apps/helloworld/aarch64.toml qemu -q ./apps/helloworld/qemu-aarch64.toml
```

### VS Code 调试

项目包含预配置的 VS Code 调试设置：

1. 使用 F5 或 "Run and Debug" 面板
2. 选择 "KDebug cppdbg" 配置
3. 会自动启动后台 QEMU（通过 tasks.json 中的 `qemu debug` 任务）
4. GDB multiarch 连接到 localhost:1234

**Windows 环境额外要求**：

- 安装 MSYS2：`pacman -S mingw-w64-ucrt-x86_64-toolchain`
- 将 `gdb-multiarch.exe` 添加到 PATH（默认在 `C:\msys64\ucrt64\bin`）

### 配置文件系统

首次运行 `ostool` 后会生成 `.project.toml`，包含默认构建配置。项目使用分层配置：

- `.project.toml` - 全局默认配置
- `build-config/aarch64.toml` - AArch64 架构配置
- `build-config/loongarch64.toml` - LoongArch64 架构配置
- `*/qemu-*.toml` - 各应用的 QEMU 运行配置

配置文件格式定义在 `.build-schema.json` 中。

## 架构总览

### 分层设计

```
应用层 (apps/, test-suit/)
    ↓
平台运行时 (platform/sparreal-rt/)
    ↓
内核核心 (crates/sparreal-kernel/)
    ↓
硬件抽象层 (crates/somehal/)
    ↓
架构实现 (somehal/src/arch/)
```

### 启动流程

1. **引导入口**: `platform/sparreal-rt/src/lib.rs` 的 `main()` 函数（通过 `#[somehal::entry]` 宏）
2. **内核启动**: 调用 `sparreal_kernel::run_kernel()`
3. **HAL 初始化**: `sparreal-kernel/src/hal/setup.rs` 的 `start_kernel()`
   - 初始化日志系统
   - 设置内存分配器（KAlloc 双堆）
   - 初始化页表
   - 初始化定时器
   - 启用中断
4. **用户入口**: 调用 `__sparreal_main()` （用户应用定义）
5. **关闭**: `al::platform::shutdown()`

### 核心抽象层 (HAL)

内核通过 trait 系统与硬件解耦，这些 trait 定义在 `crates/sparreal-kernel/src/hal/al.rs`：

#### `Memory` trait

- 地址转换（virt_to_phys, phys_to_virt）
- 页表管理
- 内存映射访问

#### `Platform` trait

- 平台特定的初始化（post_allocator）
- 中断控制（irq_set_enabled）
- 系统关闭（shutdown）

#### `Cpu` trait

- CPU ID 获取
- 本地中断控制
- 系统定时器管理

#### `Console` trait

- 早期控制台输出（early_write）
- 早期输入（early_read）

### 异步执行器

位于 `crates/sparreal-kernel/src/os/async/executor.rs`：

**关键特性**：

- 单 CPU 优先级调度（BinaryHeap）
- Wake 任务优先执行
- 超时优先级提升（默认 1 秒阈值）
- 中断安全（IrqSpinlock）
- 全局唤醒队列（GLOBAL_WAKEUP_QUEUE）

**任务生命周期**：

- `TaskId` - 唯一标识符
- `TaskState` - 就绪、运行、等待
- `TaskPriority` - 优先级（唤醒时间 > 基础优先级）
- `TaskMetadata` - 任务元数据

### 内存管理

#### KAlloc 双堆设计

`crates/sparreal-kernel/src/os/mem/allocator.rs`：

- **32 位堆**: 用于小对象分配（< 2GB）
- **64 位堆**: 用于大对象分配（>= 2GB）
- 伙伴系统算法
- 集成 `buddy_system` crate

#### 页表管理

`crates/page-table-generic/` 提供通用页表抽象：

- `FrameAllocator` trait - 物理帧分配
- `TableGeneric` - 多级页表实现
- 支持大页映射（AArch64 2MB/1GB 页）

### 硬件抽象层 (somehal)

`crates/somehal/` 提供跨架构硬件抽象：

#### ArchTrait 统一接口

定义在 `somehal/src/lib.rs`，包含：

- 页表操作（创建、切换、查询）
- 地址转换（虚拟/物理/I/O）
- 定时器管理（频率、中断、读取）
- 中断控制（全局和本地）
- 系统电源管理

#### 架构实现

- `somehal/src/arch/aarch64/` - AArch64 支持
  - EL1/EL2 异常级别
  - ARMv8-MMU 页表
  - Generic Timer 集成
- `somehal/src/arch/loongarch64/` - LoongArch64 支持
  - CSR 寄存器操作
  - TLB 管理
  - 常量映射配置

### 中断系统

`crates/sparreal-kernel/src/os/irq/`：

- `NoIrqGuard` - RAII 风格的中断禁用守卫
- `BTreeMap` 存储中断处理器
- 支持软中断和硬件中断
- 通过 `handle_irq()` 分发到注册的处理器

### 日志系统

`crates/sparreal-kernel/src/os/logger.rs`：

- KLogger 彩色日志实现
- 支持 emoji 图标（🔹 INFO、⚠️ WARN、❌ ERROR）
- 集成 `log` crate

## 模块依赖关系

```
sparreal-rt (平台运行时)
  ├─→ sparreal-kernel (内核核心)
  │     ├─→ somehal (硬件抽象)
  │     ├─→ page-table-generic (页表)
  │     ├─→ kernutil (工具)
  │     └─→ dma-api (DMA)
  └─→ somehal (架构实现)
        ├─→ kasm-aarch64 (AArch64 汇编)
        └─→ somehal-macros (宏)
```

## 关键约定

### 代码风格

- 使用 Rust 2024 Edition
- `#![no_std]` 环境
- 错误处理使用 `thiserror` 和 `anyhow`
- 并发使用 `spin::Mutex` 和自定义 `IrqSpinlock`

### 平台适配

添加新平台需要实现 HAL trait（见 `README.md`）：

```rust
use sparreal_kernel::hal::al;
use sparreal_macros::api_impl;

pub struct PlatformImpl;

#[api_impl]
impl Platform for PlatformImpl {
    unsafe fn wait_for_interrupt() {
        aarch64_cpu::asm::wfi();
    }
    // ... 其他方法
}
```

### 特性标志

- `sparreal-rt/uspace` - 用户空间模式
- `sparreal-rt/hv` - 虚拟化支持（实验性）

### 命名约定

- 外部 trait: `#[trait_ffi::def_extern_trait]` 标记
- FFI 安全类型: 使用 `kernutil::define_type!` 宏
- 平台实现: `hal_impl` 模块

## 工作空间 (Cargo Workspace)

项目是 Cargo workspace，成员：

- `apps/*` - 应用程序
- `crates/*` - 核心库和工具
- `platform/sparreal-rt` - 平台运行时
- `test-suit/*` - 测试套件

所有依赖版本和工作空间设置在根 `Cargo.toml` 中统一管理。

## 常见任务

### 添加新测试用例

1. 在 `test-suit/` 下创建新目录
2. 添加 `Cargo.toml` 和 `src/main.rs`
3. 创建架构配置文件（如 `aarch64.toml`）
4. 创建 QEMU 配置文件（如 `qemu-aarch64.toml`）
5. 使用 `ostool run -c <config> qemu -q <qemu-config>` 测试

### 修改内核功能

1. 核心代码在 `crates/sparreal-kernel/`
2. HAL 接口在 `crates/somehal/`
3. 修改后运行相关测试套件验证
4. 使用 `ostool run qemu -d` 进行调试

### 调试技巧

- 使用 `log::info!()` 等宏输出调试信息
- VS Code 断点调试需要 GDB multiarch
- 查阅 `qemu.log` 了解 QEMU 输出
- 使用 `RUST_LOG=debug` 环境变量控制日志级别

## 相关文档

- [README.md](README.md) - 项目介绍和快速开始
- 模块级 CLAUDE.md 文档位于各子目录
- 架构详细分析见现有 CLAUDE.md 的"深度挖掘结果"部分
