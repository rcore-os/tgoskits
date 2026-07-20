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

固件中 `status = "disabled"` 的节点会归一化为 inactive alias：它既不申请设备，
也不授权 I/O aperture；若 active 且已授权的节点与它描述同一物理资源，它也不会
反向给该资源打洞。只有 inactive 描述的地址仍保持未映射。

`HostPlatformSnapshot` 单独记录固件选中的 console。AArch64 进行虚拟替换时会保持
客户机可见的 UART 编程模型：PL011、packed NS16550 与 Synopsys DW-APB 使用不同
model ID，DW-APB 固定为 32 位访问和四字节寄存器步长，并生成一致的 FDT 属性。
Rockchip FIQ debugger 等固件包装节点只用于解析底层 UART，不会暴露给客户机。

### 文档

生成并查看 API 文档：

```bash
cargo doc --no-deps --open
```

在线文档：[docs.rs/axvm](https://docs.rs/axvm)

# 贡献

1. Fork 仓库并创建分支
2. 在本地运行格式化与检查
3. 运行与该 crate 相关的测试
4. 提交 PR 并确保 CI 通过

# 许可证

本项目采用 Apache License 2.0 许可证。详情见 [LICENSE](./LICENSE)。
