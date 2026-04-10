<h1 align="center">riscv-h</h1>

<p align="center">RISC-V virtualization-related registers</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/riscv-h.svg)](https://crates.io/crates/riscv-h)
[![Docs.rs](https://docs.rs/riscv-h/badge.svg)](https://docs.rs/riscv-h)
[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`riscv-h` provides RISC-V virtualization-related registers. It is maintained as part of the TGOSKits component set and is intended for Rust projects that integrate with ArceOS, AxVisor, or related low-level systems software.

## Quick Start

### Installation

Add this crate to your `Cargo.toml`:

```toml
[dependencies]
riscv-h = "0.4.0"
```

### Run Check and Test

```bash
# Enter the crate directory
cd components/riscv-h

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
use riscv_h as _;

fn main() {
    // Integrate `riscv-h` into your project here.
}
```

### Documentation

Generate and view API documentation:

```bash
cargo doc --no-deps --open
```

Online documentation: [docs.rs/riscv-h](https://docs.rs/riscv-h)

# Contributing

1. Fork the repository and create a branch
2. Run local format and checks
3. Run local tests relevant to this crate
4. Submit a PR and ensure CI passes

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](./LICENSE) for details.
