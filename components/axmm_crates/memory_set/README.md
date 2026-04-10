<h1 align="center">ax-memory-set</h1>

<p align="center">Data structures and operations for managing memory mappings</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/ax-memory-set.svg)](https://crates.io/crates/ax-memory-set)
[![Docs.rs](https://docs.rs/ax-memory-set/badge.svg)](https://docs.rs/ax-memory-set)
[![Rust](https://img.shields.io/badge/edition-2021-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`ax-memory-set` provides Data structures and operations for managing memory mappings. It is maintained as part of the TGOSKits component set and is intended for Rust projects that integrate with ArceOS, AxVisor, or related low-level systems software.


> ax-memory-set was derived from https://github.com/arceos-org/axmm_crates

## Quick Start

### Installation

Add this crate to your `Cargo.toml`:

```toml
[dependencies]
ax-memory-set = "0.6.1"
```

### Run Check and Test

```bash
# Enter the crate directory
cd components/axmm_crates/memory_set

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
use ax_memory_set as _;

fn main() {
    // Integrate `ax-memory-set` into your project here.
}
```

### Documentation

Generate and view API documentation:

```bash
cargo doc --no-deps --open
```

Online documentation: [docs.rs/ax-memory-set](https://docs.rs/ax-memory-set)

# Contributing

1. Fork the repository and create a branch
2. Run local format and checks
3. Run local tests relevant to this crate
4. Submit a PR and ensure CI passes

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](./LICENSE) for details.
