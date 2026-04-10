<h1 align="center">ArceOS</h1>

<p align="center">一个使用 Rust 编写的实验性模块化操作系统</p>

<div align="center">

[![GitHub](https://img.shields.io/badge/repo-ArceOS-black.svg)](https://github.com/arceos-org/arceos)
[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

[English](README.md) | 中文

# 介绍

`ArceOS` 是一个使用 Rust 编写的实验性模块化操作系统。该工作区包含 ArceOS 在本仓库中的核心模块、API、用户态风格库以及示例程序。

## 工作区结构

- `modules/`：内核与运行时模块
- `api/`：对应用暴露的公共接口
- `ulib/`：`axstd`、`axlibc` 等库
- `examples/`：可运行的示例程序

## 快速开始

```bash
# 进入工作区目录
cd os/arceos

# 代码格式化
cargo fmt --all

# 运行 clippy
cargo clippy --workspace --all-targets --all-features

# 运行测试
cargo test --workspace --all-features
```

## 仓库信息

- 源代码仓库：[arceos-org/arceos](https://github.com/arceos-org/arceos)
- 文档站点：[arceos-org.github.io/arceos](https://arceos-org.github.io/arceos/)

# 许可证

本项目采用 Apache License 2.0 许可证。详情见 [LICENSE](./LICENSE)。
