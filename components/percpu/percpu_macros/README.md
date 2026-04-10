<h1 align="center">ax-percpu-macros</h1>

<p align="center">Macros to define and access a per-CPU data structure</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/ax-percpu-macros.svg)](https://crates.io/crates/ax-percpu-macros)
[![Docs.rs](https://docs.rs/ax-percpu-macros/badge.svg)](https://docs.rs/ax-percpu-macros)
[![Rust](https://img.shields.io/badge/edition-2021-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`ax-percpu-macros` provides Macros to define and access a per-CPU data structure. It is maintained as part of the TGOSKits component set and is intended for Rust projects that integrate with ArceOS, AxVisor, or related low-level systems software.


> ax-percpu-macros was derived from https://github.com/arceos-org/percpu

## Quick Start

### Installation

Add this crate to your `Cargo.toml`:

```toml
[dependencies]
ax-percpu-macros = "0.4.3"
```

### Run Check and Test

```bash
# Enter the crate directory
cd components/percpu/percpu_macros

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
use ax_percpu_macros as _;

fn main() {
    // Integrate `ax-percpu-macros` into your project here.
}
```

### Documentation

Generate and view API documentation:

```bash
cargo doc --no-deps --open
```

Online documentation: [docs.rs/ax-percpu-macros](https://docs.rs/ax-percpu-macros)

# Contributing

1. Fork the repository and create a branch
2. Run local format and checks
3. Run local tests relevant to this crate
4. Submit a PR and ensure CI passes

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](./LICENSE) for details.
