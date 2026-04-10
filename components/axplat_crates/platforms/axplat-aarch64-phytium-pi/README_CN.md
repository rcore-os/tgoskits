<h1 align="center">ax-plat-aarch64-phytium-pi</h1>

<p align="center">Implementation of `axplat` hardware abstraction layer for Phytium Pi board</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/ax-plat-aarch64-phytium-pi.svg)](https://crates.io/crates/ax-plat-aarch64-phytium-pi)
[![Docs.rs](https://docs.rs/ax-plat-aarch64-phytium-pi/badge.svg)](https://docs.rs/ax-plat-aarch64-phytium-pi)
[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

[English](README.md) | 中文

# 介绍

`ax-plat-aarch64-phytium-pi` 提供了 Implementation of `axplat` hardware abstraction layer for Phytium Pi board。它是 TGOSKits 组件集合的一部分，可用于集成 ArceOS、AxVisor 及相关底层系统软件的 Rust 项目。


> ax-plat-aarch64-phytium-pi 派生自 https://github.com/arceos-org/axplat_crates

## 快速开始

### 添加依赖

在 `Cargo.toml` 中加入：

```toml
[dependencies]
ax-plat-aarch64-phytium-pi = "0.5.1"
```

### 检查与测试

```bash
# 进入 crate 目录
cd components/axplat_crates/platforms/axplat-aarch64-phytium-pi

# 代码格式化
cargo fmt --all

# 运行 clippy
cargo clippy --all-targets --all-features

# 运行测试
cargo test --all-features

# 生成文档
cargo doc --no-deps
```

## 集成方式

### 示例

```rust
use ax_plat_aarch64_phytium_pi as _;

fn main() {
    // 在这里将 `ax-plat-aarch64-phytium-pi` 集成到你的项目中。
}
```

### 文档

生成并查看 API 文档：

```bash
cargo doc --no-deps --open
```

在线文档：[docs.rs/ax-plat-aarch64-phytium-pi](https://docs.rs/ax-plat-aarch64-phytium-pi)

# 贡献

1. Fork 仓库并创建分支
2. 在本地运行格式化与检查
3. 运行与该 crate 相关的测试
4. 提交 PR 并确保 CI 通过

# 许可证

本项目采用 Apache License 2.0 许可证。详情见 [LICENSE](./LICENSE)。
