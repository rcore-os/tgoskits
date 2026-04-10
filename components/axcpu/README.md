<h1 align="center">ax-cpu</h1>

<p align="center">Privileged instruction and structure abstractions for various CPU architectures</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/ax-cpu.svg)](https://crates.io/crates/ax-cpu)
[![Docs.rs](https://docs.rs/ax-cpu/badge.svg)](https://docs.rs/ax-cpu)
[![Rust](https://img.shields.io/badge/edition-2021-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`ax-cpu` provides Privileged instruction and structure abstractions for various CPU architectures. It is maintained as part of the TGOSKits component set and is intended for Rust projects that integrate with ArceOS, AxVisor, or related low-level systems software.


> ax-cpu was derived from https://github.com/arceos-org/cpu

## Quick Start

### Installation

Add this crate to your `Cargo.toml`:

```toml
[dependencies]
ax-cpu = "0.5.0"
```

### Run Check and Test

```bash
# Enter the crate directory
cd components/axcpu

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
use ax_cpu as _;

fn main() {
    // Integrate `ax-cpu` into your project here.
}
```

### Documentation

Generate and view API documentation:

```bash
cargo doc --no-deps --open
```

Online documentation: [docs.rs/ax-cpu](https://docs.rs/ax-cpu)

# Contributing

1. Fork the repository and create a branch
2. Run local format and checks
3. Run local tests relevant to this crate
4. Submit a PR and ensure CI passes

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](./LICENSE) for details.
