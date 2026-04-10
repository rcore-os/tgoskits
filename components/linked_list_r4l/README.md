<h1 align="center">ax-linked-list-r4l</h1>

<p align="center">Linked lists that supports arbitrary removal in constant time</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/ax-linked-list-r4l.svg)](https://crates.io/crates/ax-linked-list-r4l)
[![Docs.rs](https://docs.rs/ax-linked-list-r4l/badge.svg)](https://docs.rs/ax-linked-list-r4l)
[![Rust](https://img.shields.io/badge/edition-2021-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`ax-linked-list-r4l` provides Linked lists that supports arbitrary removal in constant time. It is maintained as part of the TGOSKits component set and is intended for Rust projects that integrate with ArceOS, AxVisor, or related low-level systems software.


> ax-linked-list-r4l was derived from https://github.com/arceos-org/linked_list_r4l

## Quick Start

### Installation

Add this crate to your `Cargo.toml`:

```toml
[dependencies]
ax-linked-list-r4l = "0.5.0"
```

### Run Check and Test

```bash
# Enter the crate directory
cd components/linked_list_r4l

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
use ax_linked_list_r4l as _;

fn main() {
    // Integrate `ax-linked-list-r4l` into your project here.
}
```

### Documentation

Generate and view API documentation:

```bash
cargo doc --no-deps --open
```

Online documentation: [docs.rs/ax-linked-list-r4l](https://docs.rs/ax-linked-list-r4l)

# Contributing

1. Fork the repository and create a branch
2. Run local format and checks
3. Run local tests relevant to this crate
4. Submit a PR and ensure CI passes

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](./LICENSE) for details.
