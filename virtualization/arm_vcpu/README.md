<h1 align="center">arm_vcpu</h1>

<p align="center">OS-neutral AArch64 vCPU core</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/arm_vcpu.svg)](https://crates.io/crates/arm_vcpu)
[![Docs.rs](https://docs.rs/arm_vcpu/badge.svg)](https://docs.rs/arm_vcpu)
[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`arm_vcpu` provides an OS-neutral AArch64 vCPU core. It owns EL2 guest entry/exit, guest register state, trap decode, and hardware virtualization register semantics. Host OS and VMM policy is supplied through `ArmHostOps`; AxVM integration lives in `virtualization/axvm/src/arch/aarch64`.

## Quick Start

### Installation

Add this crate to your `Cargo.toml`:

```toml
[dependencies]
arm_vcpu = "0.5.0"
```

### Run Check and Test

```bash
# Enter the crate directory
cd virtualization/arm_vcpu

# Format code
cargo fmt --all

# Run the workspace clippy flow
cargo xtask clippy --package arm_vcpu

# Run host-runnable contract tests
cargo test -p arm_vcpu --test dependency_contract_test

# Build documentation
cargo doc --no-deps
```

## Integration

### Example

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

### Documentation

Generate and view API documentation:

```bash
cargo doc --no-deps --open
```

Online documentation: [docs.rs/arm_vcpu](https://docs.rs/arm_vcpu)

# Contributing

1. Fork the repository and create a branch
2. Run local format and checks
3. Run local tests relevant to this crate
4. Submit a PR and ensure CI passes

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](./LICENSE) for details.
