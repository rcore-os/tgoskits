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

Guest PSCI 调用和受限的 SMCCC architecture discovery 由 VM 内部实现。未注册的
SMCCC owner 默认被拒绝并返回 `SMCCC_RET_NOT_SUPPORTED`；如果 VMM 需要
SCMI 或其他安全固件协议，必须提供经所有权校验的 mediated capability，
不能把任意 guest SMC 参数直接转发给 host firmware。

被 trap 的 GICv3 common CPU Interface 访问会解码为
`ArmGicCpuInterfaceRegister` 和强类型 `ArmVmExit`。VMM 处理
`ICC_CTLR_EL1`、`ICC_PMR_EL1` 和 `ICC_RPR_EL1` 时不再依赖裸 sysreg 编码；
`ICC_DIR_EL1` 仍使用独立 deactivation exit，因为它负责一个原子的中断状态转换。

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
use arm_vcpu::{ArmHostOps, ArmVcpu, ArmVcpuCreateConfig, ArmVcpuResult};

struct MyHost;

impl ArmHostOps for MyHost {
    fn handle_current_host_irq() {}
}

fn build_vcpu() -> ArmVcpuResult<ArmVcpu<MyHost>> {
    ArmVcpu::<MyHost>::new(0, 0, ArmVcpuCreateConfig::default())
}
```

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
