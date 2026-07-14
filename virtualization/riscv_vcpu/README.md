<h1 align="center">riscv_vcpu</h1>

<p align="center">OS-neutral RISC-V vCPU core</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/riscv_vcpu.svg)](https://crates.io/crates/riscv_vcpu)
[![Docs.rs](https://docs.rs/riscv_vcpu/badge.svg)](https://docs.rs/riscv_vcpu)
[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`riscv_vcpu` provides the RISC-V Hypervisor Extension vCPU core used by Axvisor.
The crate owns VM entry/exit, CSR state, SBI handling, guest-page-fault decode,
timer pending state, and local RISC-V exit/error/value types.

The crate is OS-neutral. Host integration is provided through `RiscvHostOps`,
and AxVM-specific traits, errors, device policy, and runtime IRQ handling live in
the AxVM RISC-V adapter.

# Target Support

The crate is compiled only for `target_arch = "riscv64"`. On other targets it is
an empty crate. Consumers should depend on it through target-specific
dependencies:

```toml
[target.'cfg(target_arch = "riscv64")'.dependencies]
riscv_vcpu = "0.5"
```

# Public API

- `RiscvVcpu<H>` / `RiscvVCpu<H>` / `RISCVVCpu<H>`
- `RiscvPerCpu` / `RISCVPerCpu`
- `RiscvHostOps`
- `RiscvVmExit`
- `RiscvVcpuError` / `RiscvVcpuResult`
- `RiscvGuestPhysAddr`, `RiscvGuestVirtAddr`, `RiscvHostPhysAddr`, `RiscvHostVirtAddr`
- `RiscvAccessWidth`, `RiscvAccessFlags`, `RiscvNestedPagingConfig`

# Validation

Typical local checks from the workspace root:

```bash
cargo test -p riscv_vcpu --tests
cargo check -p riscv_vcpu --target riscv64gc-unknown-none-elf
cargo xtask clippy --package riscv_vcpu
```

End-to-end behavior is validated through Axvisor RISC-V smoke tests.

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](./LICENSE) for details.
