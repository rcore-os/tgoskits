<h1 align="center">axconfig-gen</h1>

<p align="center">Workspace for TOML-based configuration generation crates</p>

<div align="center">

[![Rust](https://img.shields.io/badge/edition-2021-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`axconfig-gen` is a workspace that groups related TGOSKits components under a unified layout. It helps organize closely related crates that are typically developed, versioned, and used together.

> axconfig-gen was derived from https://github.com/arceos-org/axconfig-gen

## Workspace Members

- `axconfig-gen`
- `axconfig-macros`

## Quick Start

```bash
# Enter the workspace directory
cd components/axconfig-gen

# Format code
cargo fmt --all

# Run clippy
cargo clippy --workspace --all-targets --all-features

# Run tests
cargo test --workspace --all-features
```

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](./LICENSE) for details.
