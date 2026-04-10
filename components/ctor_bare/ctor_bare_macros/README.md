<h1 align="center">ax-ctor-bare-macros</h1>

<p align="center">Macros for registering constructor functions for Rust under no_std</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/ax-ctor-bare-macros.svg)](https://crates.io/crates/ax-ctor-bare-macros)
[![Docs.rs](https://docs.rs/ax-ctor-bare-macros/badge.svg)](https://docs.rs/ax-ctor-bare-macros)
[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`ax-ctor-bare-macros` provides Macros for registering constructor functions for Rust under no_std. It is maintained as part of the TGOSKits component set and is intended for Rust projects that integrate with ArceOS, AxVisor, or related low-level systems software.


> ax-ctor-bare-macros was derived from https://github.com/arceos-org/ctor_bare

## Quick Start

### Installation

Add this crate to your `Cargo.toml`:

```toml
[dependencies]
ax-ctor-bare-macros = "0.4.1"
```

### Run Check and Test

```bash
# Enter the crate directory
cd components/ctor_bare/ctor_bare_macros

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
use ax_ctor_bare_macros as _;

fn main() {
    // Integrate `ax-ctor-bare-macros` into your project here.
}
```

### Documentation

Generate and view API documentation:

```bash
cargo doc --no-deps --open
```

Online documentation: [docs.rs/ax-ctor-bare-macros](https://docs.rs/ax-ctor-bare-macros)

# Contributing

1. Fork the repository and create a branch
2. Run local format and checks
3. Run local tests relevant to this crate
4. Submit a PR and ensure CI passes

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](./LICENSE) for details.
