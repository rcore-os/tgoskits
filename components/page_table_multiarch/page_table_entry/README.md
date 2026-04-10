<h1 align="center">ax-page-table-entry</h1>

<p align="center">Page table entry definition for various hardware architectures</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/ax-page-table-entry.svg)](https://crates.io/crates/ax-page-table-entry)
[![Docs.rs](https://docs.rs/ax-page-table-entry/badge.svg)](https://docs.rs/ax-page-table-entry)
[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`ax-page-table-entry` provides Page table entry definition for various hardware architectures. It is maintained as part of the TGOSKits component set and is intended for Rust projects that integrate with ArceOS, AxVisor, or related low-level systems software.


> ax-page-table-entry was derived from https://github.com/arceos-org/page_table_multiarch

## Quick Start

### Installation

Add this crate to your `Cargo.toml`:

```toml
[dependencies]
ax-page-table-entry = "0.8.1"
```

### Run Check and Test

```bash
# Enter the crate directory
cd components/page_table_multiarch/page_table_entry

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
use ax_page_table_entry as _;

fn main() {
    // Integrate `ax-page-table-entry` into your project here.
}
```

### Documentation

Generate and view API documentation:

```bash
cargo doc --no-deps --open
```

Online documentation: [docs.rs/ax-page-table-entry](https://docs.rs/ax-page-table-entry)

# Contributing

1. Fork the repository and create a branch
2. Run local format and checks
3. Run local tests relevant to this crate
4. Submit a PR and ensure CI passes

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](./LICENSE) for details.
