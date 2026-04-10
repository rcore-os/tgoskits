<h1 align="center">x86_vlapic</h1>

<p align="center">x86 Virtual Local APIC</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/x86_vlapic.svg)](https://crates.io/crates/x86_vlapic)
[![Docs.rs](https://docs.rs/x86_vlapic/badge.svg)](https://docs.rs/x86_vlapic)
[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`x86_vlapic` provides x86 Virtual Local APIC. It is maintained as part of the TGOSKits component set and is intended for Rust projects that integrate with ArceOS, AxVisor, or related low-level systems software.

## Quick Start

### Installation

Add this crate to your `Cargo.toml`:

```toml
[dependencies]
x86_vlapic = "0.4.2"
```

### Run Check and Test

```bash
# Enter the crate directory
cd components/x86_vlapic

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
use x86_vlapic as _;

fn main() {
    // Integrate `x86_vlapic` into your project here.
}
```

### Documentation

Generate and view API documentation:

```bash
cargo doc --no-deps --open
```

Online documentation: [docs.rs/x86_vlapic](https://docs.rs/x86_vlapic)

# Contributing

1. Fork the repository and create a branch
2. Run local format and checks
3. Run local tests relevant to this crate
4. Submit a PR and ensure CI passes

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](./LICENSE) for details.
