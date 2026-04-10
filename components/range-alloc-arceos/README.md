<h1 align="center">range-alloc-arceos</h1>

<p align="center">Generic range allocator</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/range-alloc-arceos.svg)](https://crates.io/crates/range-alloc-arceos)
[![Docs.rs](https://docs.rs/range-alloc-arceos/badge.svg)](https://docs.rs/range-alloc-arceos)
[![Rust](https://img.shields.io/badge/edition-2018-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`range-alloc-arceos` provides Generic range allocator. It is maintained as part of the TGOSKits component set and is intended for Rust projects that integrate with ArceOS, AxVisor, or related low-level systems software.

## Quick Start

### Installation

Add this crate to your `Cargo.toml`:

```toml
[dependencies]
range-alloc-arceos = "0.3.4"
```

### Run Check and Test

```bash
# Enter the crate directory
cd components/range-alloc-arceos

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
use range_alloc_arceos as _;

fn main() {
    // Integrate `range-alloc-arceos` into your project here.
}
```

### Documentation

Generate and view API documentation:

```bash
cargo doc --no-deps --open
```

Online documentation: [docs.rs/range-alloc-arceos](https://docs.rs/range-alloc-arceos)

# Contributing

1. Fork the repository and create a branch
2. Run local format and checks
3. Run local tests relevant to this crate
4. Submit a PR and ensure CI passes

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](./LICENSE) for details.
