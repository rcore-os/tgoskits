<h1 align="center">axvm</h1>

<p align="center">面向 ArceOS 虚拟化形态的虚拟机资源与运行期管理组件</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/axvm.svg)](https://crates.io/crates/axvm)
[![Docs.rs](https://docs.rs/axvm/badge.svg)](https://docs.rs/axvm)
[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

[English](README.md) | 中文

# 介绍

`axvm` 负责虚拟机、虚拟处理器、客户机地址空间、设备运行期和架构适配的组织。它是 TGOSKits 组件集合的一部分，可用于集成 ArceOS、Axvisor 及相关底层系统软件项目。

架构适配采用分层能力接口：通用代码只依赖所有架构都满足的统一入口，只有部分架构具备的处理器启动能力由独立接口表达，公共行为由默认方法提供，单一实现保留在具体架构路径。设备侧继续使用既有的访问、轮询、中断、生命周期和授权能力；资源需求与解析结果仍由封闭数据类型表达，不进行机械拆分。

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

- [AxVM 分层能力接口设计](../../docs/design/axvm-capability-layering.md) —— 架构共同能力、部分能力、默认行为和设备体系边界的权威说明。
- [Axvisor 解析后设备图与客户机固件](../../docs/design/axvisor-resolved-device-graph.md) —— 设备图、资源规划、注册事务与固件事实来源。

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
