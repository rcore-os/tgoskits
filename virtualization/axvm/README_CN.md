<h1 align="center">axvm</h1>

<p align="center">Virtual Machine resource management crate for ArceOS's hypervisor variant</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/axvm.svg)](https://crates.io/crates/axvm)
[![Docs.rs](https://docs.rs/axvm/badge.svg)](https://docs.rs/axvm)
[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

[English](README.md) | 中文

# 介绍

`axvm` 提供了 Virtual Machine resource management crate for ArceOS's hypervisor variant。它是 TGOSKits 组件集合的一部分，可用于集成 ArceOS、AxVisor 及相关底层系统软件的 Rust 项目。

## 快速开始

### 添加依赖

在 `Cargo.toml` 中加入：

```toml
[dependencies]
axvm = "0.5.0"
```

### 检查与测试

```bash
# 进入 crate 目录
cd virtualization/axvm

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
use axvm as _;

fn main() {
    // 在这里将 `axvm` 集成到你的项目中。
}
```

### 文档

生成并查看 API 文档：

```bash
cargo doc --no-deps --open
```

在线文档：[docs.rs/axvm](https://docs.rs/axvm)

### VM 生命周期

- [VM 生命周期模型](docs/lifecycle.md) —— 权威生命周期模型参考（状态、转换、请求与完成语义）。
- [VM 生命周期实现者视图](docs/lifecycle-internals.md) —— 实现者视角内部细节（生命周期 × runtime
  两个维度、runtime 生命周期、不可观测状态与锁语义）。

# 贡献

1. Fork 仓库并创建分支
2. 在本地运行格式化与检查
3. 运行与该 crate 相关的测试
4. 提交 PR 并确保 CI 通过

# 许可证

本项目采用 Apache License 2.0 许可证。详情见 [LICENSE](./LICENSE)。
