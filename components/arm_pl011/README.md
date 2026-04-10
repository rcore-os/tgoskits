<h1 align="center">ax-arm-pl011</h1>

<p align="center">ARM Uart pl011 register definitions and basic operations</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/ax-arm-pl011.svg)](https://crates.io/crates/ax-arm-pl011)
[![Docs.rs](https://docs.rs/ax-arm-pl011/badge.svg)](https://docs.rs/ax-arm-pl011)
[![Rust](https://img.shields.io/badge/edition-2021-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`ax-arm-pl011` provides ARM Uart pl011 register definitions and basic operations. It is maintained as part of the TGOSKits component set and is intended for Rust projects that integrate with ArceOS, AxVisor, or related low-level systems software.


> ax-arm-pl011 was derived from https://github.com/arceos-org/arm_pl011

## Quick Start

### Installation

Add this crate to your `Cargo.toml`:

```toml
[dependencies]
ax-arm-pl011 = "0.3.0"
```

### Run Check and Test

```bash
# Enter the crate directory
cd components/arm_pl011

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
use ax_arm_pl011 as _;

fn main() {
    // Integrate `ax-arm-pl011` into your project here.
}
```

### Documentation

Generate and view API documentation:

```bash
cargo doc --no-deps --open
```

Online documentation: [docs.rs/ax-arm-pl011](https://docs.rs/ax-arm-pl011)

# Contributing

1. Fork the repository and create a branch
2. Run local format and checks
3. Run local tests relevant to this crate
4. Submit a PR and ensure CI passes

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](./LICENSE) for details.
