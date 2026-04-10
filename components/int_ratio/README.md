<h1 align="center">ax-int-ratio</h1>

<p align="center">The type of ratios represented by two integers</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/ax-int-ratio.svg)](https://crates.io/crates/ax-int-ratio)
[![Docs.rs](https://docs.rs/ax-int-ratio/badge.svg)](https://docs.rs/ax-int-ratio)
[![Rust](https://img.shields.io/badge/edition-2021-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`ax-int-ratio` provides The type of ratios represented by two integers. It is maintained as part of the TGOSKits component set and is intended for Rust projects that integrate with ArceOS, AxVisor, or related low-level systems software.


> ax-int-ratio was derived from https://github.com/arceos-org/int_ratio

## Quick Start

### Installation

Add this crate to your `Cargo.toml`:

```toml
[dependencies]
ax-int-ratio = "0.3.2"
```

### Run Check and Test

```bash
# Enter the crate directory
cd components/int_ratio

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
use ax_int_ratio as _;

fn main() {
    // Integrate `ax-int-ratio` into your project here.
}
```

### Documentation

Generate and view API documentation:

```bash
cargo doc --no-deps --open
```

Online documentation: [docs.rs/ax-int-ratio](https://docs.rs/ax-int-ratio)

# Contributing

1. Fork the repository and create a branch
2. Run local format and checks
3. Run local tests relevant to this crate
4. Submit a PR and ensure CI passes

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](./LICENSE) for details.
