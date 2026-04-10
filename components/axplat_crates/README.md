<h1 align="center">axplat_crates</h1>

<p align="center">Workspace for hardware platform abstraction crates built on axplat</p>

<div align="center">

[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`axplat_crates` is a workspace that groups related TGOSKits components under a unified layout. It helps organize closely related crates that are typically developed, versioned, and used together.

> axplat_crates was derived from https://github.com/arceos-org/axplat_crates

## Workspace Members

- `axplat`
- `axplat-macros`
- `cargo-axplat`
- `platforms/axplat-x86-pc`
- `platforms/axplat-aarch64-peripherals`
- `platforms/axplat-aarch64-qemu-virt`
- `platforms/axplat-aarch64-raspi`
- `platforms/axplat-aarch64-bsta1000b`
- `platforms/axplat-aarch64-phytium-pi`
- `platforms/axplat-riscv64-qemu-virt`
- `platforms/axplat-loongarch64-qemu-virt`
- `examples/hello-kernel`
- `examples/irq-kernel`
- `examples/smp-kernel`

## Quick Start

```bash
# Enter the workspace directory
cd components/axplat_crates

# Format code
cargo fmt --all

# Run clippy
cargo clippy --workspace --all-targets --all-features

# Run tests
cargo test --workspace --all-features
```

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](./LICENSE) for details.
