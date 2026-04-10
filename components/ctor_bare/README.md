<h1 align="center">ctor_bare</h1>

<p align="center">Workspace for constructor registration crates in no_std environments</p>

<div align="center">

[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`ctor_bare` is a workspace that groups related TGOSKits components under a unified layout. It helps organize closely related crates that are typically developed, versioned, and used together.

> ctor_bare was derived from https://github.com/arceos-org/ctor_bare

## Workspace Members

- `ctor_bare`
- `ctor_bare_macros`

## Quick Start

```bash
# Enter the workspace directory
cd components/ctor_bare

# Format code
cargo fmt --all

# Run clippy
cargo clippy --workspace --all-targets --all-features

# Run tests
cargo test --workspace --all-features
```

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](./LICENSE) for details.
