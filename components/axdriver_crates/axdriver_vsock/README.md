<h1 align="center">ax-driver-vsock</h1>

<p align="center">Common traits and types for vsock drivers</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/ax-driver-vsock.svg)](https://crates.io/crates/ax-driver-vsock)
[![Docs.rs](https://docs.rs/ax-driver-vsock/badge.svg)](https://docs.rs/ax-driver-vsock)
[![Rust](https://img.shields.io/badge/edition-2021-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`ax-driver-vsock` provides Common traits and types for vsock drivers. It is maintained as part of the TGOSKits component set and is intended for Rust projects that integrate with ArceOS, AxVisor, or related low-level systems software.


> ax-driver-vsock was derived from https://github.com/arceos-org/axdriver_crates

## Quick Start

### Installation

Add this crate to your `Cargo.toml`:

```toml
[dependencies]
ax-driver-vsock = "0.3.4"
```

### Run Check and Test

```bash
# Enter the crate directory
cd components/axdriver_crates/axdriver_vsock

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
use ax_driver_vsock as _;

fn main() {
    // Integrate `ax-driver-vsock` into your project here.
}
```

### Documentation

Generate and view API documentation:

```bash
cargo doc --no-deps --open
```

Online documentation: [docs.rs/ax-driver-vsock](https://docs.rs/ax-driver-vsock)

# Contributing

1. Fork the repository and create a branch
2. Run local format and checks
3. Run local tests relevant to this crate
4. Submit a PR and ensure CI passes

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](./LICENSE) for details.
