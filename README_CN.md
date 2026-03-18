<h1 align="center">aarch64_sysreg</h1>

<p align="center">AArch64 系统寄存器类型定义库</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/aarch64_sysreg.svg)](https://crates.io/crates/aarch64_sysreg)
[![Docs.rs](https://docs.rs/aarch64_sysreg/badge.svg)](https://docs.rs/aarch64_sysreg)
[![Rust](https://img.shields.io/badge/edition-2021-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://github.com/arceos-org/aarch64_sysreg/blob/main/LICENSE)

</div>

[English](README.md) | 中文

# 简介

AArch64 系统寄存器类型定义库，提供 ARM64 架构中操作类型、寄存器类型和系统寄存器的枚举定义。支持 `#![no_std]`，可用于裸机和操作系统内核开发。

本库导出三个核心枚举类型：

- **`OperationType`** — AArch64 指令操作类型（1000+ 种指令）
- **`RegistersType`** — 通用寄存器与向量寄存器（W/X/V/B/H/S/D/Q/Z/P 等）
- **`SystemRegType`** — 系统寄存器（调试、跟踪、性能计数、系统控制等）

每个类型均实现了 `Display`、`From<usize>`、`LowerHex`、`UpperHex` trait。

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
git clone https://github.com/arceos-org/aarch64_sysreg.git
cd aarch64_sysreg

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
aarch64_sysreg = "0.1.1"
```

### 使用示例

```rust
use aarch64_sysreg::{OperationType, RegistersType, SystemRegType};

fn main() {
    // 操作类型：枚举变体与数值互转
    let op = OperationType::ADD;
    println!("{}", op);                      // ADD
    println!("0x{:x}", op);                  // 0x6
    println!("0x{:X}", op);                  // 0x6

    let op_from = OperationType::from(0x6);
    assert_eq!(op_from, OperationType::ADD);

    // 寄存器类型
    let reg = RegistersType::X0;
    println!("{}", reg);                     // X0
    let reg_from = RegistersType::from(0x22);
    assert_eq!(reg_from, RegistersType::X0);

    // 系统寄存器
    let sys_reg = SystemRegType::MDSCR_EL1;
    println!("{}", sys_reg);                 // MDSCR_EL1
    println!("0x{:x}", sys_reg);             // 0x240004
}
```

### 文档

生成并查看 API 文档：

```bash
cargo doc --no-deps --open
```

在线文档：[docs.rs/aarch64_sysreg](https://docs.rs/aarch64_sysreg)

# 贡献

1. Fork 仓库并创建分支
2. 运行本地检查：`./scripts/check.sh`
3. 运行本地测试：`./scripts/test.sh`
4. 提交 PR 并通过 CI 检查

# 协议

本项目采用 Apache License, Version 2.0 许可证。详见 [LICENSE](LICENSE) 文件。
