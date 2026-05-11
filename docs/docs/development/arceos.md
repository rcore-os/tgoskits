# ArceOS 开发指南

ArceOS 既是可以单独运行的模块化 Unikernel，也是 StarryOS 与 Axvisor 共享的基础能力层。本文聚焦**改了什么之后该如何验证**的开发闭环和调试技巧。

> 仓库布局、架构分层和模块体系见 [ArceOS 架构](/docs/architecture/arceos)。  
> 最短命令和快速启动见 [ArceOS 快速上手](/docs/quickstart/arceos)。  
> 构建系统总览见 [build-system.md](/docs/build/overview)。

## 常见开发动作

### 修改基础组件或模块

如果你改的是：

- `components/axerrno`、`components/kspin`、`components/page_table_multiarch`
- 或 `os/arceos/modules/axhal`、`axtask`、`axdriver`、`axnet`、`axfs`

建议先跑最小消费者：

```bash
cargo xtask arceos qemu --package ax-helloworld --arch aarch64
```

如果改动依赖特定功能，再换对应示例：

```bash
cargo xtask arceos qemu --package ax-httpclient --arch aarch64
```

### 新增 feature 或暴露给应用

常见接线顺序：

1. 在 `os/arceos/modules/*` 完成或接入实现
2. 在 `os/arceos/api/axfeat` 暴露 feature
3. 需要给应用直接用时，再接到 `os/arceos/ulib/axstd` 或 `axlibc`

### 添加新示例应用

新增示例放在 `os/arceos/examples/<name>/`。Rust 示例的最小 `Cargo.toml`：

```toml
[package]
name = "myapp"
version = "0.1.0"
edition.workspace = true

[dependencies]
ax-std.workspace = true
```

最小 `src/main.rs`：

```rust
#![cfg_attr(feature = "ax-std", no_std)]
#![cfg_attr(feature = "ax-std", no_main)]

#[cfg(feature = "ax-std")]
use ax_std::println;

#[cfg_attr(feature = "ax-std", unsafe(no_mangle))]
fn main() {
    println!("Hello from myapp!");
}
```

验证：

```bash
cargo xtask arceos qemu --package myapp --arch aarch64
```

仓库中也包含 C 示例（`helloworld-c`、`httpclient-c`、`httpserver-c`）。

### 添加或修改平台

需要关注以下目录：

- `components/axplat_crates/platforms/*`：各架构平台实现
- `platform/axplat-dyn`：动态平台抽象
- `platform/x86-qemu-q35`、`platform/riscv64-qemu-virt`

验证时显式指定架构；切平台或 feature 需修改对应 `build-<target>.toml` 或回到 `os/arceos/Makefile`。

## 调试建议

### 看更详细的运行日志

```bash
cd os/arceos
make A=examples/helloworld ARCH=riscv64 LOG=debug run
```

### 启动 GDB 调试

```bash
cd os/arceos
make A=examples/helloworld ARCH=riscv64 debug
```

### 什么时候优先用根目录入口

- 你要验证集成行为
- 你要和 `test-suit`、StarryOS 或 Axvisor 的共享依赖对齐
- 你希望命令风格和 CI 更接近

## 继续往哪里读

- [ArceOS 架构](/docs/architecture/arceos): 分层、feature 装配、模块体系
- [components.md](/docs/development/components): 共享依赖如何接到三个系统
- [build-system.md](/docs/build/overview): xtask、Makefile 与 workspace 边界
- [starryos-guide.md](/docs/development/starryos): 如果改动波及 StarryOS
- [axvisor-guide.md](/docs/development/axvisor): 如果改动波及 Axvisor
