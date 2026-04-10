<h1 align="center">ax-sched</h1>

<p align="center">Various scheduler algorithms in a unified interface</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/ax-sched.svg)](https://crates.io/crates/ax-sched)
[![Docs.rs](https://docs.rs/ax-sched/badge.svg)](https://docs.rs/ax-sched)
[![Rust](https://img.shields.io/badge/edition-2021-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`ax-sched` provides Various scheduler algorithms in a unified interface. It is maintained as part of the TGOSKits component set and is intended for Rust projects that integrate with ArceOS, AxVisor, or related low-level systems software.


> ax-sched was derived from https://github.com/arceos-org/sched

## Quick Start

### Installation

Add this crate to your `Cargo.toml`:

```toml
[dependencies]
ax-sched = "0.5.1"
```

### Run Check and Test

```bash
# Enter the crate directory
cd components/axsched

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
use ax_sched as _;

fn main() {
    // Integrate `ax-sched` into your project here.
}
```

### Documentation

Generate and view API documentation:

```bash
cargo doc --no-deps --open
```

Online documentation: [docs.rs/ax-sched](https://docs.rs/ax-sched)

# Contributing

1. Fork the repository and create a branch
2. Run local format and checks
3. Run local tests relevant to this crate
4. Submit a PR and ensure CI passes

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](./LICENSE) for details.
