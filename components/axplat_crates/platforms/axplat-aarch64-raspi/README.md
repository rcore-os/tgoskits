<h1 align="center">ax-plat-aarch64-raspi</h1>

<p align="center">Implementation of `axplat` hardware abstraction layer for Raspberry Pi 4B board</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/ax-plat-aarch64-raspi.svg)](https://crates.io/crates/ax-plat-aarch64-raspi)
[![Docs.rs](https://docs.rs/ax-plat-aarch64-raspi/badge.svg)](https://docs.rs/ax-plat-aarch64-raspi)
[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`ax-plat-aarch64-raspi` provides Implementation of `axplat` hardware abstraction layer for Raspberry Pi 4B board. It is maintained as part of the TGOSKits component set and is intended for Rust projects that integrate with ArceOS, AxVisor, or related low-level systems software.


> ax-plat-aarch64-raspi was derived from https://github.com/arceos-org/axplat_crates

## Quick Start

### Installation

Add this crate to your `Cargo.toml`:

```toml
[dependencies]
ax-plat-aarch64-raspi = "0.5.1"
```

### Run Check and Test

```bash
# Enter the crate directory
cd components/axplat_crates/platforms/axplat-aarch64-raspi

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
use ax_plat_aarch64_raspi as _;

fn main() {
    // Integrate `ax-plat-aarch64-raspi` into your project here.
}
```

### Documentation

Generate and view API documentation:

```bash
cargo doc --no-deps --open
```

Online documentation: [docs.rs/ax-plat-aarch64-raspi](https://docs.rs/ax-plat-aarch64-raspi)

# Contributing

1. Fork the repository and create a branch
2. Run local format and checks
3. Run local tests relevant to this crate
4. Submit a PR and ensure CI passes

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](./LICENSE) for details.
