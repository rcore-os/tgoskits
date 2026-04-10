<h1 align="center">ax-lazyinit</h1>

<p align="center">Initialize a static value lazily</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/ax-lazyinit.svg)](https://crates.io/crates/ax-lazyinit)
[![Docs.rs](https://docs.rs/ax-lazyinit/badge.svg)](https://docs.rs/ax-lazyinit)
[![Rust](https://img.shields.io/badge/edition-2021-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`ax-lazyinit` provides Initialize a static value lazily. It is maintained as part of the TGOSKits component set and is intended for Rust projects that integrate with ArceOS, AxVisor, or related low-level systems software.


> ax-lazyinit was derived from https://github.com/arceos-org/lazyinit

## Quick Start

### Installation

Add this crate to your `Cargo.toml`:

```toml
[dependencies]
ax-lazyinit = "0.4.2"
```

### Run Check and Test

```bash
# Enter the crate directory
cd components/ax-lazyinit

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
use ax_lazyinit as _;

fn main() {
    // Integrate `ax-lazyinit` into your project here.
}
```

### Documentation

Generate and view API documentation:

```bash
cargo doc --no-deps --open
```

Online documentation: [docs.rs/ax-lazyinit](https://docs.rs/ax-lazyinit)

# Contributing

1. Fork the repository and create a branch
2. Run local format and checks
3. Run local tests relevant to this crate
4. Submit a PR and ensure CI passes

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](./LICENSE) for details.
