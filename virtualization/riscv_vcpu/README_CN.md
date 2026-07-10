<h1 align="center">riscv_vcpu</h1>

<p align="center">OS-neutral RISC-V vCPU core</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/riscv_vcpu.svg)](https://crates.io/crates/riscv_vcpu)
[![Docs.rs](https://docs.rs/riscv_vcpu/badge.svg)](https://docs.rs/riscv_vcpu)
[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

[English](README.md) | 中文

# 介绍

`riscv_vcpu` 提供 Axvisor 使用的 RISC-V Hypervisor Extension vCPU core。
该 crate 负责 VM entry/exit、CSR 状态、SBI 处理、guest page fault 解码、
timer pending state，以及本 crate 专用的 RISC-V exit/error/value types。

该 crate 是 OS-neutral 功能库。宿主侧能力通过 `RiscvHostOps` 注入；
AxVM trait、错误转换、设备策略和运行时 IRQ 处理都位于 AxVM 的 RISC-V adapter。

# 目标架构

该 crate 只在 `target_arch = "riscv64"` 下编译；其他 target 下为空 crate。
消费者应使用 target-specific dependency：

```toml
[target.'cfg(target_arch = "riscv64")'.dependencies]
riscv_vcpu = "0.5"
```

# 公共 API

- `RiscvVcpu<H>` / `RiscvVCpu<H>` / `RISCVVCpu<H>`
- `RiscvPerCpu` / `RISCVPerCpu`
- `RiscvHostOps`
- `RiscvVmExit`
- `RiscvVcpuError` / `RiscvVcpuResult`
- `RiscvGuestPhysAddr`, `RiscvGuestVirtAddr`, `RiscvHostPhysAddr`, `RiscvHostVirtAddr`
- `RiscvAccessWidth`, `RiscvAccessFlags`, `RiscvNestedPagingConfig`

# 验证

常用本地检查从 workspace root 执行：

```bash
cargo test -p riscv_vcpu --tests
cargo check -p riscv_vcpu --target riscv64gc-unknown-none-elf
cargo xtask clippy --package riscv_vcpu
```

端到端行为通过 Axvisor RISC-V smoke tests 验证。

# 许可证

本项目采用 Apache License 2.0 许可证。详情见 [LICENSE](./LICENSE)。
