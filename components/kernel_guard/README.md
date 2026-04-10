<h1 align="center">ax-kernel-guard</h1>

<p align="center">RAII wrappers to create a critical section with local IRQs or preemption disabled</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/ax-kernel-guard.svg)](https://crates.io/crates/ax-kernel-guard)
[![Docs.rs](https://docs.rs/ax-kernel-guard/badge.svg)](https://docs.rs/ax-kernel-guard)
[![Rust](https://img.shields.io/badge/edition-2021-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`ax-kernel-guard` provides RAII wrappers to create a critical section with local IRQs or preemption disabled. It is maintained as part of the TGOSKits component set and is intended for Rust projects that integrate with ArceOS, AxVisor, or related low-level systems software.


> ax-kernel-guard was derived from https://github.com/arceos-org/kernel_guard

## Quick Start

### Installation

Add this crate to your `Cargo.toml`:

```toml
[dependencies]
ax-kernel-guard = "0.3.3"
```

### Run Check and Test

```bash
# Enter the crate directory
cd components/kernel_guard

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
use ax_kernel_guard as _;

fn main() {
    // Integrate `ax-kernel-guard` into your project here.
}
```

### Documentation

Generate and view API documentation:

```bash
cargo doc --no-deps --open
```

Online documentation: [docs.rs/ax-kernel-guard](https://docs.rs/ax-kernel-guard)

# Contributing

1. Fork the repository and create a branch
2. Run local format and checks
3. Run local tests relevant to this crate
4. Submit a PR and ensure CI passes

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](./LICENSE) for details.
