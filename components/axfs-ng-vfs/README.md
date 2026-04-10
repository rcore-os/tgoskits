<h1 align="center">axfs-ng-vfs</h1>

<p align="center">Virtual filesystem layer for ArceOS</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/axfs-ng-vfs.svg)](https://crates.io/crates/axfs-ng-vfs)
[![Docs.rs](https://docs.rs/axfs-ng-vfs/badge.svg)](https://docs.rs/axfs-ng-vfs)
[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`axfs-ng-vfs` provides Virtual filesystem layer for ArceOS. It is maintained as part of the TGOSKits component set and is intended for Rust projects that integrate with ArceOS, AxVisor, or related low-level systems software.

## Quick Start

### Installation

Add this crate to your `Cargo.toml`:

```toml
[dependencies]
axfs-ng-vfs = "0.3.1"
```

### Run Check and Test

```bash
# Enter the crate directory
cd components/axfs-ng-vfs

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
use axfs_ng_vfs as _;

fn main() {
    // Integrate `axfs-ng-vfs` into your project here.
}
```

### Documentation

Generate and view API documentation:

```bash
cargo doc --no-deps --open
```

Online documentation: [docs.rs/axfs-ng-vfs](https://docs.rs/axfs-ng-vfs)

# Contributing

1. Fork the repository and create a branch
2. Run local format and checks
3. Run local tests relevant to this crate
4. Submit a PR and ensure CI passes

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](./LICENSE) for details.
