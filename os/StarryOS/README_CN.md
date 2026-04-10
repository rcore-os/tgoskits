<h1 align="center">StarryOS</h1>

<p align="center">基于 ArceOS unikernel 构建的 Linux 兼容操作系统内核</p>

<div align="center">

[![GitHub](https://img.shields.io/badge/repo-StarryOS-black.svg)](https://github.com/Starry-OS/StarryOS)
[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

[English](README.md) | 中文

# 介绍

`StarryOS` 是一个构建在 ArceOS 之上的 Linux 兼容操作系统内核。该工作区包含用于构建和运行 StarryOS 内核的核心包。

## 工作区成员

- `starryos`
- `kernel`

## 快速开始

```bash
# 进入工作区目录
cd os/StarryOS

# 代码格式化
cargo fmt --all

# 运行 clippy
cargo clippy --workspace --all-targets --all-features

# 运行测试
cargo test --workspace --all-features
```

## 仓库信息

- 源代码仓库：[Starry-OS/StarryOS](https://github.com/Starry-OS/StarryOS)
- 项目主页：[Starry-OS](https://github.com/Starry-OS)

# 许可证

本项目采用 Apache License 2.0 许可证。详情见 [LICENSE](./LICENSE)。
