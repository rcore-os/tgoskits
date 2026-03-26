<h1 align="center">arm_vcpu</h1>

<p align="center">AArch64 Virtual CPU Implementation for ArceOS Hypervisor</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/arm_vcpu.svg)](https://crates.io/crates/arm_vcpu)
[![Docs.rs](https://docs.rs/arm_vcpu/badge.svg)](https://docs.rs/arm_vcpu)
[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://github.com/arceos-hypervisor/arm_vcpu/blob/main/LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`arm_vcpu` provides an AArch64 virtual CPU implementation for the ArceOS Hypervisor stack. It focuses on EL2-based virtualization on ARMv8-A platforms and offers the core data structures and handlers needed to create, configure, and run guest vCPUs in `#![no_std]` environments.

This crate exports the following core types:

- **`Aarch64VCpu`** — the main AArch64 vCPU implementation
- **`Aarch64VCpuCreateConfig`** — configuration used when creating a vCPU
- **`Aarch64VCpuSetupConfig`** — runtime setup options such as interrupt and timer passthrough
- **`Aarch64PerCpu`** — per-CPU support for the AArch64 virtualization environment
- **`TrapFrame`** — guest trap context frame used during exception handling

It also provides **`has_hardware_support()`** to query whether the current platform supports the required virtualization capability.

## Quick Start

### Requirements

- AArch64 platform
- EL2 (Hypervisor mode) support
- Rust nightly toolchain
- Rust components: rust-src, clippy, rustfmt

```bash
# Install rustup (if not installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install nightly toolchain and components
rustup install nightly
rustup component add rust-src clippy rustfmt --toolchain nightly
```

### Run Check and Test

```bash
# 1. Clone the repository
git clone https://github.com/arceos-hypervisor/arm_vcpu.git
cd arm_vcpu

# 2. Code check (format + clippy + build + doc generation)
./scripts/check.sh

# 3. Run tests
# Run all tests (unit tests + integration tests)
./scripts/test.sh

# Run unit tests only
./scripts/test.sh unit

# Run integration tests only
./scripts/test.sh integration

# List all available test suites
./scripts/test.sh list

# Restrict target selection
./scripts/test.sh all --targets x86_64-unknown-linux-gnu
```

## Integration

### Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
arm_vcpu = "0.3.0"
```

### Example

```rust
use arm_vcpu::{Aarch64VCpu, Aarch64VCpuCreateConfig, Aarch64VCpuSetupConfig, has_hardware_support};
use axaddrspace::{GuestPhysAddr, HostPhysAddr};
use axvcpu::AxArchVCpu;

fn create_vcpu() {
    if !has_hardware_support() {
        return;
    }

    let mut vcpu = Aarch64VCpu::new(
        0,
        0,
        Aarch64VCpuCreateConfig {
            mpidr_el1: 0,
            dtb_addr: 0,
        },
    )
    .unwrap();

    vcpu.setup(Aarch64VCpuSetupConfig {
        passthrough_interrupt: false,
        passthrough_timer: false,
    })
    .unwrap();

    vcpu.set_entry(GuestPhysAddr::from_usize(0x4000)).unwrap();
    vcpu.set_ept_root(HostPhysAddr::from_usize(0x8000)).unwrap();
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
2. Run local check: `./scripts/check.sh`
3. Run local tests: `./scripts/test.sh`
4. Submit PR and pass CI checks

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](https://github.com/arceos-hypervisor/arm_vcpu/blob/main/LICENSE) for details.
