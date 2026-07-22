<h1 align="center">axvmconfig</h1>

<p align="center">A simple VM configuration tool for ArceOS-Hypervisor</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/axvmconfig.svg)](https://crates.io/crates/axvmconfig)
[![Docs.rs](https://docs.rs/axvmconfig/badge.svg)](https://docs.rs/axvmconfig)
[![Rust](https://img.shields.io/badge/edition-2021-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`axvmconfig` strictly parses Axvisor guest machine requests. The public schema
uses `[machine]`, `[[memory.regions]]`, `[devices]`, and
`[[devices.virtual]]`; unknown and removed legacy fields are rejected.

`interrupts_passthrough` is an optional passthrough-machine boolean that is
immediately normalized into `PhysicalInterruptPolicy`. It applies only to
assigned physical IRQ sources: `false` selects mediated software inputs and
`true` selects ownership-checked, hardware-backed forwarding. Virtual-device
IRQs remain software-backed under either policy. The field is not accepted for
Virtual machines, even when set to `false`.

## Quick Start

### Installation

Add this crate to your `Cargo.toml`:

```toml
[dependencies]
axvmconfig = "0.4.2"
```

### Run Check and Test

```bash
# Enter the crate directory
cd virtualization/axvmconfig

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

```toml
[machine]
mode = "virtual"
firmware = "auto"

[[memory.regions]]
guest_base = 0x80000000
size = 0x40000000
permissions = "rwx"
backing = { kind = "allocate" }

[devices]
disable_defaults = []
deny = []
```

Memory backing kinds are `allocate`, `identity-allocate`, `host`, `shared`, and
`reserved`. `identity-allocate` is available to x86_64 and AArch64 Passthrough
machines: it allocates zeroed VM-owned RAM and chooses the allocation's host
physical address as the guest address so an assigned device can DMA without an
IOMMU. Its configured `guest_base` is therefore a zero placeholder, not a fixed
range beginning at zero. Fixed guest memory ranges must not overlap.

### Documentation

Generate and view API documentation:

```bash
cargo doc --no-deps --open
```

Online documentation: [docs.rs/axvmconfig](https://docs.rs/axvmconfig)

# Contributing

1. Fork the repository and create a branch
2. Run local format and checks
3. Run local tests relevant to this crate
4. Submit a PR and ensure CI passes

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](./LICENSE) for details.
