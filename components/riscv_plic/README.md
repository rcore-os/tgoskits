<h1 align="center">ax-riscv-plic</h1>

<p align="center">RISC-V platform-level interrupt controller (PLIC) register definitions and basic operations</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/ax-riscv-plic.svg)](https://crates.io/crates/ax-riscv-plic)
[![Docs.rs](https://docs.rs/ax-riscv-plic/badge.svg)](https://docs.rs/ax-riscv-plic)
[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`ax-riscv-plic` provides RISC-V platform-level interrupt controller (PLIC) register definitions and basic operations. It is maintained as part of the TGOSKits component set and is intended for Rust projects that integrate with ArceOS, AxVisor, or related low-level systems software.


> ax-riscv-plic was derived from https://github.com/arceos-org/riscv_plic

## Quick Start

### Installation

Add this crate to your `Cargo.toml`:

```toml
[dependencies]
ax-riscv-plic = "0.4.0"
```

### Run Check and Test

```bash
# Enter the crate directory
cd components/riscv_plic

# Format code
cargo fmt --all

# Run clippy
cargo clippy --all-targets --all-features

# Run tests
cargo test --all-features

# Build documentation
cargo doc --no-deps
```

## Integration

### Example

```rust
use ax_riscv_plic as _;

fn main() {
    // Integrate `ax-riscv-plic` into your project here.
}
```

### Documentation

Generate and view API documentation:

```bash
cargo doc --no-deps --open
```

Online documentation: [docs.rs/ax-riscv-plic](https://docs.rs/ax-riscv-plic)

# Contributing

1. Fork the repository and create a branch
2. Run local format and checks
3. Run local tests relevant to this crate
4. Submit a PR and ensure CI passes

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](./LICENSE) for details.
