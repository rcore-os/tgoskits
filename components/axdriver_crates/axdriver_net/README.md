<h1 align="center">ax-driver-net</h1>

<p align="center">Common traits and types for network device (NIC) drivers</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/ax-driver-net.svg)](https://crates.io/crates/ax-driver-net)
[![Docs.rs](https://docs.rs/ax-driver-net/badge.svg)](https://docs.rs/ax-driver-net)
[![Rust](https://img.shields.io/badge/edition-2021-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`ax-driver-net` provides Common traits and types for network device (NIC) drivers. It is maintained as part of the TGOSKits component set and is intended for Rust projects that integrate with ArceOS, AxVisor, or related low-level systems software.


> ax-driver-net was derived from https://github.com/arceos-org/axdriver_crates

## Quick Start

### Installation

Add this crate to your `Cargo.toml`:

```toml
[dependencies]
ax-driver-net = "0.3.4"
```

### Run Check and Test

```bash
# Enter the crate directory
cd components/axdriver_crates/axdriver_net

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
use ax_driver_net as _;

fn main() {
    // Integrate `ax-driver-net` into your project here.
}
```

### Documentation

Generate and view API documentation:

```bash
cargo doc --no-deps --open
```

Online documentation: [docs.rs/ax-driver-net](https://docs.rs/ax-driver-net)

# Contributing

1. Fork the repository and create a branch
2. Run local format and checks
3. Run local tests relevant to this crate
4. Submit a PR and ensure CI passes

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](./LICENSE) for details.
