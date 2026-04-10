<h1 align="center">axplat_crates</h1>

<p align="center">基于 axplat 的硬件平台抽象 crate 工作区</p>

<div align="center">

[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

[English](README.md) | 中文

# 介绍

`axplat_crates` 是一个工作区，用于将相关的 TGOSKits 组件放在统一的目录结构下，便于协同开发、版本管理与组合使用。

> axplat_crates 派生自 https://github.com/arceos-org/axplat_crates

## 工作区成员

- `axplat`
- `axplat-macros`
- `cargo-axplat`
- `platforms/axplat-x86-pc`
- `platforms/axplat-aarch64-peripherals`
- `platforms/axplat-aarch64-qemu-virt`
- `platforms/axplat-aarch64-raspi`
- `platforms/axplat-aarch64-bsta1000b`
- `platforms/axplat-aarch64-phytium-pi`
- `platforms/axplat-riscv64-qemu-virt`
- `platforms/axplat-loongarch64-qemu-virt`
- `examples/hello-kernel`
- `examples/irq-kernel`
- `examples/smp-kernel`

## 快速开始

```bash
# 进入工作区目录
cd components/axplat_crates

# 代码格式化
cargo fmt --all

# 运行 clippy
cargo clippy --workspace --all-targets --all-features

# 运行测试
cargo test --workspace --all-features
```

# 许可证

本项目采用 Apache License 2.0 许可证。详情见 [LICENSE](./LICENSE)。
