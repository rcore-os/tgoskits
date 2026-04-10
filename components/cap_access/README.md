<h1 align="center">ax-cap-access</h1>

<p align="center">Provide basic capability-based access control to objects</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/ax-cap-access.svg)](https://crates.io/crates/ax-cap-access)
[![Docs.rs](https://docs.rs/ax-cap-access/badge.svg)](https://docs.rs/ax-cap-access)
[![Rust](https://img.shields.io/badge/edition-2021-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`ax-cap-access` provides Provide basic capability-based access control to objects. It is maintained as part of the TGOSKits component set and is intended for Rust projects that integrate with ArceOS, AxVisor, or related low-level systems software.


> ax-cap-access was derived from https://github.com/arceos-org/cap_access

## Quick Start

### Installation

Add this crate to your `Cargo.toml`:

```toml
[dependencies]
ax-cap-access = "0.3.0"
```

### Run Check and Test

```bash
# Enter the crate directory
cd components/cap_access

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
use ax_cap_access as _;

fn main() {
    // Integrate `ax-cap-access` into your project here.
}
```

### Documentation

Generate and view API documentation:

```bash
cargo doc --no-deps --open
```

Online documentation: [docs.rs/ax-cap-access](https://docs.rs/ax-cap-access)

# Contributing

1. Fork the repository and create a branch
2. Run local format and checks
3. Run local tests relevant to this crate
4. Submit a PR and ensure CI passes

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](./LICENSE) for details.
