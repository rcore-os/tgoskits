<h1 align="center">starry-vm</h1>

<p align="center">Virtual memory management library for Starry OS</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/starry-vm.svg)](https://crates.io/crates/starry-vm)
[![Docs.rs](https://docs.rs/starry-vm/badge.svg)](https://docs.rs/starry-vm)
[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`starry-vm` provides Virtual memory management library for Starry OS. It is maintained as part of the TGOSKits component set and is intended for Rust projects that integrate with ArceOS, AxVisor, or related low-level systems software.

## Quick Start

### Installation

Add this crate to your `Cargo.toml`:

```toml
[dependencies]
starry-vm = "0.5.0"
```

### Run Check and Test

```bash
# Enter the crate directory
cd components/starry-vm

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
use starry_vm as _;

fn main() {
    // Integrate `starry-vm` into your project here.
}
```

### Documentation

Generate and view API documentation:

```bash
cargo doc --no-deps --open
```

Online documentation: [docs.rs/starry-vm](https://docs.rs/starry-vm)

# Contributing

1. Fork the repository and create a branch
2. Run local format and checks
3. Run local tests relevant to this crate
4. Submit a PR and ensure CI passes

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](./LICENSE) for details.
