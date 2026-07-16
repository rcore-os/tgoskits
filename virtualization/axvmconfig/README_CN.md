<h1 align="center">axvmconfig</h1>

<p align="center">A simple VM configuration tool for ArceOS-Hypervisor</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/axvmconfig.svg)](https://crates.io/crates/axvmconfig)
[![Docs.rs](https://docs.rs/axvmconfig/badge.svg)](https://docs.rs/axvmconfig)
[![Rust](https://img.shields.io/badge/edition-2021-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

[English](README.md) | 中文

# 介绍

`axvmconfig` 严格解析 Axvisor 的 VM 机型请求。公开配置使用 `[machine]`、
`[[memory.regions]]`、`[devices]` 和 `[[devices.virtual]]`；未知字段和已删除的旧字段会
直接报错。

## 快速开始

### 添加依赖

在 `Cargo.toml` 中加入：

```toml
[dependencies]
axvmconfig = "0.4.2"
```

### 检查与测试

```bash
# 进入 crate 目录
cd virtualization/axvmconfig

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

```toml
[machine]
mode = "virtual"
firmware = "auto"

[[memory.regions]]
guest_base = 0x80000000
size = 0x40000000
permissions = "rwx"
backing = { kind = "allocate" }

[devices]
disable_defaults = []
deny = []
```

内存 backing 支持 `allocate`、`identity-allocate`、`host`、`shared` 和 `reserved`。
`identity-allocate` 仅允许用于 x86_64 Passthrough VM：它分配清零的 VM-owned RAM，
并以分配所得 HPA 作为 GPA，供无 IOMMU 的透传设备 DMA；因此配置里的 `guest_base`
必须写成零占位符。所有固定 GPA 内存区域都必须互不重叠。

### 文档

生成并查看 API 文档：

```bash
cargo doc --no-deps --open
```

在线文档：[docs.rs/axvmconfig](https://docs.rs/axvmconfig)

# 贡献

1. Fork 仓库并创建分支
2. 在本地运行格式化与检查
3. 运行与该 crate 相关的测试
4. 提交 PR 并确保 CI 通过

# 许可证

本项目采用 Apache License 2.0 许可证。详情见 [LICENSE](./LICENSE)。
