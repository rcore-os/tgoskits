<h1 align="center">aarch64_sysreg</h1>

<p align="center">AArch64 System Register Type Definitions</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/aarch64_sysreg.svg)](https://crates.io/crates/aarch64_sysreg)
[![Docs.rs](https://docs.rs/aarch64_sysreg/badge.svg)](https://docs.rs/aarch64_sysreg)
[![Rust](https://img.shields.io/badge/edition-2021-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://github.com/arceos-org/aarch64_sysreg/blob/main/LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

A library providing type definitions for AArch64 system registers, including operation types, register types, and system register enumerations for the ARM64 architecture. Supports `#![no_std]` for bare-metal and OS kernel development.

This library exports three core enumeration types:

- **`OperationType`** — AArch64 instruction operation types (1000+ instructions)
- **`RegistersType`** — General-purpose and vector registers (W/X/V/B/H/S/D/Q/Z/P, etc.)
- **`SystemRegType`** — System registers (debug, trace, performance counters, system control, etc.)

Each type implements `Display`, `From<usize>`, `LowerHex`, and `UpperHex` traits.

## Quick Start

### Requirements

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
git clone https://github.com/arceos-org/aarch64_sysreg.git
cd aarch64_sysreg

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

# Specify unit test target
./scripts/test.sh unit --unit-targets x86_64-unknown-linux-gnu
```

## Integration

### Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
aarch64_sysreg = "0.1.1"
```

### Example

```rust
use aarch64_sysreg::{OperationType, RegistersType, SystemRegType};

fn main() {
    // Operation type: enum variant and value conversion
    let op = OperationType::ADD;
    println!("{}", op);                      // ADD
    println!("0x{:x}", op);                  // 0x6
    println!("0x{:X}", op);                  // 0x6

    let op_from = OperationType::from(0x6);
    assert_eq!(op_from, OperationType::ADD);

    // Register type
    let reg = RegistersType::X0;
    println!("{}", reg);                     // X0
    let reg_from = RegistersType::from(0x22);
    assert_eq!(reg_from, RegistersType::X0);

    // System register
    let sys_reg = SystemRegType::MDSCR_EL1;
    println!("{}", sys_reg);                 // MDSCR_EL1
    println!("0x{:x}", sys_reg);             // 0x240004
}
```

### Documentation

Generate and view API documentation:

```bash
cargo doc --no-deps --open
```

Online documentation: [docs.rs/aarch64_sysreg](https://docs.rs/aarch64_sysreg)

# Contributing

1. Fork the repository and create a branch
2. Run local check: `./scripts/check.sh`
3. Run local tests: `./scripts/test.sh`
4. Submit PR and pass CI checks

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) for details.
