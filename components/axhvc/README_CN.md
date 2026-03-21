<h1 align="center">axhvc</h1>

<p align="center">AxVisor HyperCall 定义库</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/axhvc.svg)](https://crates.io/crates/axhvc)
[![Docs.rs](https://docs.rs/axhvc/badge.svg)](https://docs.rs/axhvc)
[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://github.com/arceos-hypervisor/axhvc/blob/main/LICENSE)

</div>

[English](README.md) | 中文

# Introduction

`axhvc` 提供 AxVisor 中 guest 与 hypervisor 通信所需的 hypercall 定义。它是一个轻量级的 `#![no_std]` crate，适用于裸机 guest、hypervisor 以及 x86_64、RISC-V、AArch64 等平台上的底层虚拟化组件。

该库导出三个核心公开类型：

- **`HyperCallCode`** - 枚举所有受支持的 AxVisor hypercall 操作
- **`InvalidHyperCallCode`** - 表示非法数值转换为 hypercall 编码时的错误
- **`HyperCallResult`** - Hypercall 处理函数使用的 `AxResult<usize>` 类型别名

`HyperCallCode` 支持 `TryFrom<u32>` 转换，当前覆盖 hypervisor 控制和 IVC 通道管理两类操作。

## Quick Start

### Requirements

- Rust nightly 工具链
- Rust 组件：rust-src、clippy、rustfmt

```bash
# 安装 rustup（如果尚未安装）
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 安装 nightly 工具链与所需组件
rustup install nightly
rustup component add rust-src clippy rustfmt --toolchain nightly
```

### Run Check and Test

```bash
# 1. 进入仓库目录
cd axhvc

# 2. 代码检查
./scripts/check.sh

# 3. 运行测试
./scripts/test.sh
```

## Integration

### Installation

将以下依赖加入 `Cargo.toml`：

```toml
[dependencies]
axhvc = "0.2.0"
```

### Example

```rust
use axerrno::ax_err;
use axhvc::{HyperCallCode, HyperCallResult, InvalidHyperCallCode};

fn handle_hypercall(code: u32) -> Result<HyperCallResult, InvalidHyperCallCode> {
    let code = HyperCallCode::try_from(code)?;

    let result = match code {
        HyperCallCode::HypervisorDisable => Ok(0),
        HyperCallCode::HyperVisorPrepareDisable => Ok(0),
        HyperCallCode::HIVCPublishChannel => Ok(0x1000),
        _ => ax_err!(Unsupported),
    };

    Ok(result)
}

fn main() {
    let code = HyperCallCode::try_from(3u32).unwrap();
    assert_eq!(code as u32, 3);

    let result = handle_hypercall(code as u32).unwrap().unwrap();
    assert_eq!(result, 0x1000);

    let invalid = HyperCallCode::try_from(7u32);
    assert!(invalid.is_err());
}
```

### Documentation

生成并查看 API 文档：

```bash
cargo doc --no-deps --open
```

在线文档： [docs.rs/axhvc](https://docs.rs/axhvc)

# Contributing

1. Fork 仓库并创建分支
2. 本地运行检查：`./scripts/check.sh`
3. 本地运行测试：`./scripts/test.sh`
4. 提交 PR 并通过 CI 检查

# License

本项目基于 Apache License 2.0 许可证发布。详见 [LICENSE](LICENSE)。
