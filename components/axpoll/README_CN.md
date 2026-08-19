<h1 align="center">axpoll</h1>

<p align="center">与调度器无关的类型化 I/O readiness 契约</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/axpoll.svg)](https://crates.io/crates/axpoll)
[![Docs.rs](https://docs.rs/axpoll/badge.svg)](https://docs.rs/axpoll)
[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

[English](README.md) | 中文

# 介绍

`axpoll` 是 TGOSKits 使用的纯 `no_std` readiness API。它定义事件位、
readiness source、拥有所有权的注册 lease，以及类型化的 shared observer
和 exclusive consumer 注册能力；它不拥有具体 wait queue、调度器、锁实现
或 hard-IRQ 唤醒路径。

任务/延迟上下文若需要具有 Linux waitqueue 选择语义的通用注册队列，应使用
[`axpoll-set`](../axpoll-set)。OS runtime 再将队列与任务阻塞、IRQ 到任务的
投递机制组合；VFS 和设备接口只依赖本 crate 的 readiness 契约。

## 快速开始

### 添加依赖

在 `Cargo.toml` 中加入：

```toml
[dependencies]
axpoll = "0.5.4"
```

### 检查与测试

```bash
# 进入 crate 目录
cd components/axpoll

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

### 契约示例

```rust
use axpoll::{IoEvents, Pollable, SharedRegistrationSink};

struct ReadableObject;

impl Pollable for ReadableObject {
    fn poll(&self) -> IoEvents {
        IoEvents::IN
    }

    unsafe fn register_shared(
        &self,
        _sink: &mut dyn SharedRegistrationSink,
        _events: IoEvents,
    ) {
        // 有状态 source 在任务/延迟上下文中注册拥有所有权的 lease。
    }
}
```

### 文档

生成并查看 API 文档：

```bash
cargo doc --no-deps --open
```

在线文档：[docs.rs/axpoll](https://docs.rs/axpoll)

# 贡献

1. Fork 仓库并创建分支
2. 在本地运行格式化与检查
3. 运行与该 crate 相关的测试
4. 提交 PR 并确保 CI 通过

# 许可证

本项目采用 Apache License 2.0 许可证。详情见 [LICENSE](./LICENSE)。
