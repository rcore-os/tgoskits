<h1 align="center">scope-local</h1>

<p align="center">Scope local storage</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/scope-local.svg)](https://crates.io/crates/scope-local)
[![Docs.rs](https://docs.rs/scope-local/badge.svg)](https://docs.rs/scope-local)
[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

[English](README.md) | 中文

# 介绍

`scope-local` 提供了 Scope local storage。它是 TGOSKits 组件集合的一部分，可用于集成 ArceOS、AxVisor 及相关底层系统软件的 Rust 项目。

## 快速开始

### 添加依赖

在 `Cargo.toml` 中加入：

```toml
[dependencies]
scope-local = "0.3.2"
```

### 检查与测试

```bash
# 进入 crate 目录
cd components/scope-local

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

```ignore
use std::sync::Arc;

use scope_local::scope_local;

scope_local! {
    static REQUEST_COUNT: usize = 0;
    static CONTEXT: Arc<str> = Arc::from("global");
}

fn main() {
    // 嵌入系统必须先提供 ax-kspin 的 LockRuntime。
    let count = REQUEST_COUNT.with(|count| *count);
    let context = CONTEXT.clone_current();
    assert_eq!(count, 0);
    assert_eq!(&*context, "global");
}
```

当前 scope 的访问通过不可逃逸闭包完成，保证 per-CPU 查找结果不会超过
CPU pin 的生命周期。对于 `Arc` 持有的资源，应优先使用 `clone_current`，并在
克隆返回、恢复抢占后再获取可睡眠锁。已经持有 IRQ 或抢占 guard 的代码可使用
`with_pinned`；hard IRQ 只能使用不会触发惰性初始化的 `try_with_pinned`。

### 文档

生成并查看 API 文档：

```bash
cargo doc --no-deps --open
```

在线文档：[docs.rs/scope-local](https://docs.rs/scope-local)

# 贡献

1. Fork 仓库并创建分支
2. 在本地运行格式化与检查
3. 运行与该 crate 相关的测试
4. 提交 PR 并确保 CI 通过

# 许可证

本项目采用 Apache License 2.0 许可证。详情见 [LICENSE](./LICENSE)。
