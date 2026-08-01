<h1 align="center">arm_vcpu</h1>

<p align="center">OS-neutral AArch64 vCPU core</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/arm_vcpu.svg)](https://crates.io/crates/arm_vcpu)
[![Docs.rs](https://docs.rs/arm_vcpu/badge.svg)](https://docs.rs/arm_vcpu)
[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

[English](README.md) | 中文

# 介绍

`arm_vcpu` 提供 OS-neutral 的 AArch64 vCPU core。它负责 EL2 guest entry/exit、guest register state、trap decode 和硬件虚拟化寄存器语义。宿主 OS/VMM 策略通过 `ArmHostOps` 提供；AxVM 接入层位于 `virtualization/axvm/src/arch/aarch64`。

## 快速开始

### 添加依赖

在 `Cargo.toml` 中加入：

```toml
[dependencies]
arm_vcpu = "0.5.0"
```

### 检查与测试

```bash
# 进入 crate 目录
cd virtualization/arm_vcpu

# 代码格式化
cargo fmt --all

# 运行工作区 clippy 流程
cargo xtask clippy --package arm_vcpu

# 运行可在 host 上执行的 contract 测试
cargo test -p arm_vcpu --test dependency_contract_test

# 生成文档
cargo doc --no-deps
```

## 集成方式

### 示例

```rust
use arm_vcpu::{
    ArmHostOps, ArmVcpu, ArmVcpuCreateConfig, ArmVcpuResult, ArmVirtualIntId,
};

struct MyHost;

impl ArmHostOps for MyHost {
    fn current_cpu_id() -> ArmVcpuResult<usize> {
        // 该示例 host 永久固定在其唯一的逻辑 CPU 上。
        Ok(0)
    }

    fn inject_virtual_interrupt(_intid: ArmVirtualIntId) -> ArmVcpuResult {
        Ok(())
    }

    fn fetch_pending_host_irq() -> Option<usize> {
        None
    }

    fn handle_current_host_irq() {}
}

fn build_vcpu() -> ArmVcpuResult<ArmVcpu<MyHost>> {
    ArmVcpu::<MyHost>::new(0, 0, ArmVcpuCreateConfig::default())
}
```

GICv3 host 必须通过 CPU pin 或等价的不可迁移作用域实现 `current_cpu_id()`；多 CPU
host 不得返回固定值。GICv2 的 bind、unbind 和注入路径不会访问 CPU-local ICH
寄存器，因此 GICv2 host 可以保留该方法默认返回错误的实现。

GICv3 vCPU 完成 bind 后，`with_bound_ich()` 可在不可逃逸的回调中提供
`IchSession`，用于类型化 LR 访问、maintenance 快照以及受控的 UIE/TDIR 设置。
回调执行期间 vCPU 必须始终固定在其绑定的 host CPU 上；session 不暴露寄存器后端，
也不能修改任意 HCR 位。GICv2 会直接返回 `Unsupported`，且不会访问 ICH 寄存器。

### 文档

生成并查看 API 文档：

```bash
cargo doc --no-deps --open
```

在线文档：[docs.rs/arm_vcpu](https://docs.rs/arm_vcpu)

# 贡献

1. Fork 仓库并创建分支
2. 在本地运行格式化与检查
3. 运行与该 crate 相关的测试
4. 提交 PR 并确保 CI 通过

# 许可证

本项目采用 Apache License 2.0 许可证。详情见 [LICENSE](./LICENSE)。
