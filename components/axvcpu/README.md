<h1 align="center">axvcpu</h1>

<p align="center">vCPU abstraction for ArceOS hypervisor</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/axvcpu.svg)](https://crates.io/crates/axvcpu)
[![Docs.rs](https://docs.rs/axvcpu/badge.svg)](https://docs.rs/axvcpu)
[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`axvcpu` provides vCPU abstraction for ArceOS hypervisor. It is maintained as part of the TGOSKits component set and is intended for Rust projects that integrate with ArceOS, AxVisor, or related low-level systems software.

## Quick Start

### Installation

Add this crate to your `Cargo.toml`:

```toml
[dependencies]
axvcpu = "0.5.0"
```

### Run Check and Test

```bash
# Enter the crate directory
cd components/axvcpu

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
use axvcpu as _;

fn main() {
    // Integrate `axvcpu` into your project here.
}
```

### Documentation

Generate and view API documentation:

```bash
cargo doc --no-deps --open
```

Online documentation: [docs.rs/axvcpu](https://docs.rs/axvcpu)

# Contributing

1. Fork the repository and create a branch
2. Run local format and checks
3. Run local tests relevant to this crate
4. Submit a PR and ensure CI passes

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](./LICENSE) for details.
