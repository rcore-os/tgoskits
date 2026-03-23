<h1 align="center">arm_vcpu</h1>

<p align="center">面向 ArceOS Hypervisor 的 AArch64 虚拟 CPU 实现</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/arm_vcpu.svg)](https://crates.io/crates/arm_vcpu)
[![Docs.rs](https://docs.rs/arm_vcpu/badge.svg)](https://docs.rs/arm_vcpu)
[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://github.com/arceos-hypervisor/arm_vcpu/blob/main/LICENSE)

</div>

[English](README.md) | 中文

# 简介

`arm_vcpu` 为 ArceOS Hypervisor 提供 AArch64 虚拟 CPU 实现，面向 ARMv8-A 平台上的 EL2 虚拟化场景，提供创建、配置和运行 Guest vCPU 所需的核心数据结构与处理逻辑，并支持 `#![no_std]` 环境。

该库主要导出以下核心类型：

- **`Aarch64VCpu`** — AArch64 vCPU 的核心实现
- **`Aarch64VCpuCreateConfig`** — 创建 vCPU 时使用的配置
- **`Aarch64VCpuSetupConfig`** — 运行期初始化配置，例如中断和定时器透传
- **`Aarch64PerCpu`** — AArch64 虚拟化环境下的每 CPU 支持结构
- **`TrapFrame`** — Guest 异常处理使用的陷入上下文

此外还提供 **`has_hardware_support()`**，用于判断当前平台是否具备所需的虚拟化能力。

## 快速开始

### 环境要求

- AArch64 平台
- 支持 EL2（Hypervisor mode）
- Rust nightly 工具链
- Rust 组件：rust-src、clippy、rustfmt

```bash
# 如果尚未安装 rustup
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 安装 nightly 工具链和所需组件
rustup install nightly
rustup component add rust-src clippy rustfmt --toolchain nightly
```

### 运行检查与测试

```bash
# 1. 克隆仓库
git clone https://github.com/arceos-hypervisor/arm_vcpu.git
cd arm_vcpu

# 2. 运行代码检查（格式 + clippy + 编译 + 文档）
./scripts/check.sh

# 3. 运行测试
# 运行全部测试（单元测试 + 集成测试）
./scripts/test.sh

# 仅运行单元测试
./scripts/test.sh unit

# 仅运行集成测试
./scripts/test.sh integration

# 列出所有可用测试套件
./scripts/test.sh list

# 限制目标 target
./scripts/test.sh all --targets x86_64-unknown-linux-gnu
```

## 集成使用

### 安装

在 `Cargo.toml` 中添加：

```toml
[dependencies]
arm_vcpu = "0.3.0"
```

### 示例

```rust
use arm_vcpu::{Aarch64VCpu, Aarch64VCpuCreateConfig, Aarch64VCpuSetupConfig, has_hardware_support};
use axaddrspace::{GuestPhysAddr, HostPhysAddr};
use axvcpu::AxArchVCpu;

fn create_vcpu() {
    if !has_hardware_support() {
        return;
    }

    let mut vcpu = Aarch64VCpu::new(
        0,
        0,
        Aarch64VCpuCreateConfig {
            mpidr_el1: 0,
            dtb_addr: 0,
        },
    )
    .unwrap();

    vcpu.setup(Aarch64VCpuSetupConfig {
        passthrough_interrupt: false,
        passthrough_timer: false,
    })
    .unwrap();

    vcpu.set_entry(GuestPhysAddr::from_usize(0x4000)).unwrap();
    vcpu.set_ept_root(HostPhysAddr::from_usize(0x8000)).unwrap();
}
```

### 文档

生成并查看 API 文档：

```bash
cargo doc --no-deps --open
```

在线文档： [docs.rs/arm_vcpu](https://docs.rs/arm_vcpu)

# 贡献

1. Fork 仓库并创建开发分支
2. 本地运行检查：`./scripts/check.sh`
3. 本地运行测试：`./scripts/test.sh`
4. 提交 PR 并通过 CI

# 许可证

本项目基于 Apache License, Version 2.0 发布。详见 [LICENSE](LICENSE)。
