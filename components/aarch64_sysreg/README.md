<h1 align="center">aarch64_sysreg</h1>

<p align="center">Address translation of system registers</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/aarch64_sysreg.svg)](https://crates.io/crates/aarch64_sysreg)
[![Docs.rs](https://docs.rs/aarch64_sysreg/badge.svg)](https://docs.rs/aarch64_sysreg)
[![Rust](https://img.shields.io/badge/edition-2021-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`aarch64_sysreg` provides Address translation of system registers. It is maintained as part of the TGOSKits component set and is intended for Rust projects that integrate with ArceOS, AxVisor, or related low-level systems software.

## Quick Start

### Installation

Add this crate to your `Cargo.toml`:

```toml
[dependencies]
aarch64_sysreg = "0.3.1"
```

### Run Check and Test

```bash
# Enter the crate directory
cd components/aarch64_sysreg

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
use aarch64_sysreg as _;

fn main() {
    // Integrate `aarch64_sysreg` into your project here.
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
2. Run local format and checks
3. Run local tests relevant to this crate
4. Submit a PR and ensure CI passes

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](./LICENSE) for details.
