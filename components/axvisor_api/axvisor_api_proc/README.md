<h1 align="center">axvisor_api_proc</h1>

<p align="center">Procedural macros for the `axvisor_api` crate</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/axvisor_api_proc.svg)](https://crates.io/crates/axvisor_api_proc)
[![Docs.rs](https://docs.rs/axvisor_api_proc/badge.svg)](https://docs.rs/axvisor_api_proc)
[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`axvisor_api_proc` provides Procedural macros for the `axvisor_api` crate. It is maintained as part of the TGOSKits component set and is intended for Rust projects that integrate with ArceOS, AxVisor, or related low-level systems software.

## Quick Start

### Installation

Add this crate to your `Cargo.toml`:

```toml
[dependencies]
axvisor_api_proc = "0.5.0"
```

### Run Check and Test

```bash
# Enter the crate directory
cd components/axvisor_api/axvisor_api_proc

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
use axvisor_api_proc as _;

fn main() {
    // Integrate `axvisor_api_proc` into your project here.
}
```

### Documentation

Generate and view API documentation:

```bash
cargo doc --no-deps --open
```

Online documentation: [docs.rs/axvisor_api_proc](https://docs.rs/axvisor_api_proc)

# Contributing

1. Fork the repository and create a branch
2. Run local format and checks
3. Run local tests relevant to this crate
4. Submit a PR and ensure CI passes

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](./LICENSE) for details.
