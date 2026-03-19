<h1 align="center">riscv_vplic</h1>

<p align="center">RISC-V Virtual Platform-Level Interrupt Controller</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/riscv_vplic.svg)](https://crates.io/crates/riscv_vplic)
[![Docs.rs](https://docs.rs/riscv_vplic/badge.svg)](https://docs.rs/riscv_vplic)
[![Rust](https://img.shields.io/badge/edition-2021-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://github.com/arceos-hypervisor/riscv_vplic/blob/main/LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

RISC-V Virtual Platform-Level Interrupt Controller, providing a PLIC 1.0.0 compliant interrupt controller emulation for RISC-V Hypervisors. Supports `#![no_std]`, suitable for bare-metal and virtualization development.

This crate exports the following core components:

- **`VPlicGlobal`** — Virtual PLIC global controller, managing interrupt priorities, pending interrupts, and active interrupts
- **PLIC Constants** — PLIC 1.0.0 memory map constant definitions (priority, pending, enable, context control, etc.)

## Key Features

- PLIC 1.0.0 compliant memory map
- Interrupt priority, pending status, and enable management
- Context-based interrupt handling with Claim/Complete mechanism
- Integration with Hypervisor device emulation framework (implements `BaseDeviceOps` trait)

# Quick Start

### Prerequisites

- Rust nightly toolchain
- Rust components: rust-src, clippy, rustfmt

```bash
# Install rustup (if not installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install nightly toolchain and components
rustup install nightly
rustup component add rust-src clippy rustfmt --toolchain nightly
```

### Running Checks and Tests

```bash
# 1. Clone the repository
git clone https://github.com/arceos-hypervisor/riscv_vplic.git
cd riscv_vplic

# 2. Code check (format check + clippy + build + doc generation)
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

# Specify unit test target
./scripts/test.sh unit --unit-targets x86_64-unknown-linux-gnu
```

# Integration

### Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
riscv_vplic = "0.2.2"
```

### Usage Example

```rust
use riscv_vplic::VPlicGlobal;
use axaddrspace::GuestPhysAddr;

fn main() {
    // Create a virtual PLIC with 2 contexts
    let vplic = VPlicGlobal::new(
        GuestPhysAddr::from(0x0c000000),
        Some(0x4000),
        2
    );
    
    // Access PLIC properties
    println!("PLIC address: {:#x}", vplic.addr.as_usize());
    println!("PLIC size: {:#x}", vplic.size);
    println!("Contexts: {}", vplic.contexts_num);
    
    // Check interrupt bitmap status
    assert!(vplic.assigned_irqs.lock().is_empty());
    assert!(vplic.pending_irqs.lock().is_empty());
    assert!(vplic.active_irqs.lock().is_empty());
}
```

### PLIC Memory Map Constants

```rust
use riscv_vplic::*;

// Number of interrupt sources (PLIC 1.0.0 defines 1024)
assert_eq!(PLIC_NUM_SOURCES, 1024);

// Memory map offsets
assert_eq!(PLIC_PRIORITY_OFFSET, 0x000000);       // Priority registers
assert_eq!(PLIC_PENDING_OFFSET, 0x001000);        // Pending registers
assert_eq!(PLIC_ENABLE_OFFSET, 0x002000);         // Enable registers
assert_eq!(PLIC_CONTEXT_CTRL_OFFSET, 0x200000);   // Context control registers

// Context strides
assert_eq!(PLIC_ENABLE_STRIDE, 0x80);             // Enable region stride
assert_eq!(PLIC_CONTEXT_STRIDE, 0x1000);          // Context stride

// Context internal control offsets
assert_eq!(PLIC_CONTEXT_THRESHOLD_OFFSET, 0x00);  // Threshold register
assert_eq!(PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET, 0x04); // Claim/Complete register
```

### Documentation

Generate and view API documentation:

```bash
cargo doc --no-deps --open
```

Online documentation: [docs.rs/riscv_vplic](https://docs.rs/riscv_vplic)

## Related Projects

- [axdevice_base](https://github.com/arceos-hypervisor/axdevice_base) - Basic device abstraction
- [axvisor_api](https://github.com/arceos-hypervisor/axvisor_api) - Hypervisor API definitions
- [axaddrspace](https://github.com/arceos-hypervisor/axaddrspace) - Address space management

# Contributing

1. Fork the repository and create a branch
2. Run local checks: `./scripts/check.sh`
3. Run local tests: `./scripts/test.sh`
4. Submit a PR and pass CI checks

# License

This project is licensed under the Apache License, Version 2.0. See the [LICENSE](LICENSE) file for details.
