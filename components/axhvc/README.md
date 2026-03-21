<h1 align="center">axhvc</h1>

<p align="center">AxVisor HyperCall Definitions</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/axhvc.svg)](https://crates.io/crates/axhvc)
[![Docs.rs](https://docs.rs/axhvc/badge.svg)](https://docs.rs/axhvc)
[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://github.com/arceos-hypervisor/axhvc/blob/main/LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`axhvc` provides AxVisor hypercall definitions for guest-hypervisor communication. It is a lightweight `#![no_std]` crate intended for bare-metal guests, hypervisors, and low-level virtualization components across x86_64, RISC-V, and AArch64 platforms.

This library exports three core public types:

- **`HyperCallCode`** - Enumerates all supported AxVisor hypercall operations
- **`InvalidHyperCallCode`** - Represents conversion errors for invalid numeric hypercall values
- **`HyperCallResult`** - Alias of `AxResult<usize>` used by hypercall handlers

`HyperCallCode` supports `TryFrom<u32>` conversion and includes variants for hypervisor control and IVC channel management.

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
# 1. Enter the repository
cd axhvc

# 2. Code check
./scripts/check.sh

# 3. Run tests
./scripts/test.sh
```

## Integration

### Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
axhvc = "0.2.0"
```

### Example

```rust
use axerrno::ax_err;
use axhvc::{HyperCallCode, HyperCallResult, InvalidHyperCallCode};

fn handle_hypercall(code: u32) -> Result<HyperCallResult, InvalidHyperCallCode> {
    let code = HyperCallCode::try_from(code)?;

    let result = match code {
        HyperCallCode::HypervisorDisable => Ok(0),
        HyperCallCode::HyperVisorPrepareDisable => Ok(0),
        HyperCallCode::HIVCPublishChannel => Ok(0x1000),
        _ => ax_err!(Unsupported),
    };

    Ok(result)
}

fn main() {
    let code = HyperCallCode::try_from(3u32).unwrap();
    assert_eq!(code as u32, 3);

    let result = handle_hypercall(code as u32).unwrap().unwrap();
    assert_eq!(result, 0x1000);

    let invalid = HyperCallCode::try_from(7u32);
    assert!(invalid.is_err());
}
```

### Documentation

Generate and view API documentation:

```bash
cargo doc --no-deps --open
```

Online documentation: [docs.rs/axhvc](https://docs.rs/axhvc)

# Contributing

1. Fork the repository and create a branch
2. Run local check: `./scripts/check.sh`
3. Run local tests: `./scripts/test.sh`
4. Submit PR and pass CI checks

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) for details.
