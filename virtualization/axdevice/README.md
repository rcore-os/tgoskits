<h1 align="center">axdevice</h1>

<p align="center">A reusable, OS-agnostic device abstraction layer designed for virtual machines</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/axdevice.svg)](https://crates.io/crates/axdevice)
[![Docs.rs](https://docs.rs/axdevice/badge.svg)](https://docs.rs/axdevice)
[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`axdevice` provides VM-local device registration, bus dispatch, two-phase
virtual-device models, and validated interrupt-controller topology. Device
models consume named resources allocated by the VM planner and receive owned
`IrqLine` or `MsiEndpoint` objects; they never inject a vCPU interrupt by vector.

The crate is `no_std + alloc` by default. Its optional `std` feature is intended
for host fixtures and domain tests.

## Quick Start

### Installation

Add this crate to your `Cargo.toml`:

```toml
[dependencies]
axdevice = "0.4.2"
```

### Run Check and Test

```bash
# Enter the crate directory
cd virtualization/axdevice

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

```rust,ignore
use axdevice::{DeviceBuildContext, ResourceSlot};

let irq_slot = ResourceSlot::new("irq")?;
let irq = build_context.irq(&irq_slot)?;
let device = MyVirtualDevice::new(irq);
```

Controllers register wired/MSI input capabilities and vCPU bindings in one
`InterruptTopology`. Topology finalization validates controller IDs, default
selection, cascades, trigger modes, cycles, and vCPU ports before the VM runs.

### Documentation

Generate and view API documentation:

```bash
cargo doc --no-deps --open
```

Online documentation: [docs.rs/axdevice](https://docs.rs/axdevice)

# Contributing

1. Fork the repository and create a branch
2. Run local format and checks
3. Run local tests relevant to this crate
4. Submit a PR and ensure CI passes

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](./LICENSE) for details.
