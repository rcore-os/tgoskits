<h1 align="center">riscv_vplic</h1>

<p align="center">RISC-V 虚拟平台级中断控制器</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/riscv_vplic.svg)](https://crates.io/crates/riscv_vplic)
[![Docs.rs](https://docs.rs/riscv_vplic/badge.svg)](https://docs.rs/riscv_vplic)
[![Rust](https://img.shields.io/badge/edition-2021-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://github.com/arceos-hypervisor/riscv_vplic/blob/main/LICENSE)

</div>

[English](README.md) | 中文

# 简介

RISC-V 虚拟平台级中断控制器（Virtual Platform-Level Interrupt Controller），为 RISC-V Hypervisor 提供符合 PLIC 1.0.0 规范的中断控制器模拟实现。支持 `#![no_std]`，可用于裸机和虚拟化开发。

本库导出以下核心内容：

- **`VPlicGlobal`** — 虚拟 PLIC 全局控制器，管理中断优先级、待处理中断和活跃中断
- **PLIC 常量** — PLIC 1.0.0 内存映射常量定义（优先级、待处理、使能、上下文控制等）

## 主要特性

- 符合 PLIC 1.0.0 规范的内存映射
- 支持中断优先级、待处理状态和使能管理
- 基于 Context 的中断处理，支持 Claim/Complete 机制
- 与 Hypervisor 设备模拟框架集成（实现 `BaseDeviceOps` trait）

## 快速上手

### 环境要求

- Rust nightly 工具链
- Rust 组件: rust-src, clippy, rustfmt

```bash
# 安装 rustup（如未安装）
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 安装 nightly 工具链及组件
rustup install nightly
rustup component add rust-src clippy rustfmt --toolchain nightly
```

### 运行检查和测试

```bash
# 1. 克隆仓库
git clone https://github.com/arceos-hypervisor/riscv_vplic.git
cd riscv_vplic

# 2. 代码检查（格式检查 + clippy + 构建 + 文档生成）
./scripts/check.sh

# 3. 运行测试
# 运行全部测试（单元测试 + 集成测试）
./scripts/test.sh

# 仅运行单元测试
./scripts/test.sh unit

# 仅运行集成测试
./scripts/test.sh integration

# 列出所有可用的测试套件
./scripts/test.sh list

# 指定单元测试目标
./scripts/test.sh unit --unit-targets x86_64-unknown-linux-gnu
```

## 集成使用

### 安装

在 `Cargo.toml` 中添加：

```toml
[dependencies]
riscv_vplic = "0.2.2"
```

### 使用示例

```rust
use riscv_vplic::VPlicGlobal;
use axaddrspace::GuestPhysAddr;

fn main() {
    // 创建一个支持 2 个上下文的虚拟 PLIC
    let vplic = VPlicGlobal::new(
        GuestPhysAddr::from(0x0c000000),
        Some(0x4000),
        2
    );
    
    // 访问 PLIC 属性
    println!("PLIC address: {:#x}", vplic.addr.as_usize());
    println!("PLIC size: {:#x}", vplic.size);
    println!("Contexts: {}", vplic.contexts_num);
    
    // 检查中断位图状态
    assert!(vplic.assigned_irqs.lock().is_empty());
    assert!(vplic.pending_irqs.lock().is_empty());
    assert!(vplic.active_irqs.lock().is_empty());
}
```

### PLIC 内存映射常量

```rust
use riscv_vplic::*;

// 中断源数量（PLIC 1.0.0 定义为 1024）
assert_eq!(PLIC_NUM_SOURCES, 1024);

// 内存映射偏移量
assert_eq!(PLIC_PRIORITY_OFFSET, 0x000000);       // 优先级寄存器
assert_eq!(PLIC_PENDING_OFFSET, 0x001000);        // 待处理寄存器
assert_eq!(PLIC_ENABLE_OFFSET, 0x002000);         // 使能寄存器
assert_eq!(PLIC_CONTEXT_CTRL_OFFSET, 0x200000);   // 上下文控制寄存器

// Context 间距
assert_eq!(PLIC_ENABLE_STRIDE, 0x80);             // 使能区域间距
assert_eq!(PLIC_CONTEXT_STRIDE, 0x1000);          // 上下文间距

// 上下文内部控制偏移
assert_eq!(PLIC_CONTEXT_THRESHOLD_OFFSET, 0x00);  // 阈值寄存器
assert_eq!(PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET, 0x04); // Claim/Complete 寄存器
```

### 文档

生成并查看 API 文档：

```bash
cargo doc --no-deps --open
```

在线文档：[docs.rs/riscv_vplic](https://docs.rs/riscv_vplic)

## 相关项目

- [axdevice_base](https://github.com/arceos-hypervisor/axdevice_base) - 基础设备抽象
- [axvisor_api](https://github.com/arceos-hypervisor/axvisor_api) - Hypervisor API 定义
- [axaddrspace](https://github.com/arceos-hypervisor/axaddrspace) - 地址空间管理

# 贡献

1. Fork 仓库并创建分支
2. 运行本地检查：`./scripts/check.sh`
3. 运行本地测试：`./scripts/test.sh`
4. 提交 PR 并通过 CI 检查

# 协议

本项目采用 Apache License, Version 2.0 许可证。详见 [LICENSE](LICENSE) 文件。
