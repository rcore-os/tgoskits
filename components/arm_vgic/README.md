<h1 align="center">arm_vgic</h1>

<p align="center">ARM Virtual Generic Interrupt Controller (VGIC) implementation</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/arm_vgic.svg)](https://crates.io/crates/arm_vgic)
[![Docs.rs](https://docs.rs/arm_vgic/badge.svg)](https://docs.rs/arm_vgic)
[![Rust](https://img.shields.io/badge/edition-2021-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`arm_vgic` provides ARM Virtual Generic Interrupt Controller (VGIC) implementation. It is maintained as part of the TGOSKits component set and is intended for Rust projects that integrate with ArceOS, AxVisor, or related low-level systems software.

## Quick Start

### Installation

Add this crate to your `Cargo.toml`:

```toml
[dependencies]
arm_vgic = "0.4.2"
```

### Run Check and Test

```bash
# Enter the crate directory
cd components/arm_vgic

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
use arm_vgic as _;

fn main() {
    // Integrate `arm_vgic` into your project here.
}
```

### Documentation

Generate and view API documentation:

```bash
cargo doc --no-deps --open
```

Online documentation: [docs.rs/arm_vgic](https://docs.rs/arm_vgic)

# Contributing

1. Fork the repository and create a branch
2. Run local format and checks
3. Run local tests relevant to this crate
4. Submit a PR and ensure CI passes

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](./LICENSE) for details.
