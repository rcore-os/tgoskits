<h1 align="center">fxmac_rs</h1>

<p align="center">FXMAC Ethernet driver in Rust for PhytiumPi (Phytium Pi) board, supporting DMA-based packet transmission and reception</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/fxmac_rs.svg)](https://crates.io/crates/fxmac_rs)
[![Docs.rs](https://docs.rs/fxmac_rs/badge.svg)](https://docs.rs/fxmac_rs)
[![Rust](https://img.shields.io/badge/edition-2021-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`fxmac_rs` provides FXMAC Ethernet driver in Rust for PhytiumPi (Phytium Pi) board, supporting DMA-based packet transmission and reception. It is maintained as part of the TGOSKits component set and is intended for Rust projects that integrate with ArceOS, AxVisor, or related low-level systems software.

## Quick Start

### Installation

Add this crate to your `Cargo.toml`:

```toml
[dependencies]
fxmac_rs = "0.4.1"
```

### Run Check and Test

```bash
# Enter the crate directory
cd components/fxmac_rs

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
use fxmac_rs as _;

fn main() {
    // Integrate `fxmac_rs` into your project here.
}
```

### Documentation

Generate and view API documentation:

```bash
cargo doc --no-deps --open
```

Online documentation: [docs.rs/fxmac_rs](https://docs.rs/fxmac_rs)

# Contributing

1. Fork the repository and create a branch
2. Run local format and checks
3. Run local tests relevant to this crate
4. Submit a PR and ensure CI passes

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](./LICENSE) for details.
