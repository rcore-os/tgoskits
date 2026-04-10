<h1 align="center">ax-fs-ramfs</h1>

<p align="center">RAM filesystem used by ArceOS</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/ax-fs-ramfs.svg)](https://crates.io/crates/ax-fs-ramfs)
[![Docs.rs](https://docs.rs/ax-fs-ramfs/badge.svg)](https://docs.rs/ax-fs-ramfs)
[![Rust](https://img.shields.io/badge/edition-2021-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`ax-fs-ramfs` provides RAM filesystem used by ArceOS. It is maintained as part of the TGOSKits component set and is intended for Rust projects that integrate with ArceOS, AxVisor, or related low-level systems software.


> ax-fs-ramfs was derived from https://github.com/arceos-org/axfs_crates

## Quick Start

### Installation

Add this crate to your `Cargo.toml`:

```toml
[dependencies]
ax-fs-ramfs = "0.3.2"
```

### Run Check and Test

```bash
# Enter the crate directory
cd components/axfs_crates/axfs_ramfs

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
use ax_fs_ramfs as _;

fn main() {
    // Integrate `ax-fs-ramfs` into your project here.
}
```

### Documentation

Generate and view API documentation:

```bash
cargo doc --no-deps --open
```

Online documentation: [docs.rs/ax-fs-ramfs](https://docs.rs/ax-fs-ramfs)

# Contributing

1. Fork the repository and create a branch
2. Run local format and checks
3. Run local tests relevant to this crate
4. Submit a PR and ensure CI passes

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](./LICENSE) for details.
