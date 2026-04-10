<h1 align="center">axfs_crates</h1>

<p align="center">Workspace for filesystem-related crates used by ArceOS</p>

<div align="center">

[![Rust](https://img.shields.io/badge/edition-2021-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`axfs_crates` is a workspace that groups related TGOSKits components under a unified layout. It helps organize closely related crates that are typically developed, versioned, and used together.

> axfs_crates was derived from https://github.com/arceos-org/axfs_crates

## Workspace Members

- `axfs_vfs`
- `ax-fs-devfs`
- `ax-fs-ramfs`

## Quick Start

```bash
# Enter the workspace directory
cd components/axfs_crates

# Format code
cargo fmt --all

# Run clippy
cargo clippy --workspace --all-targets --all-features

# Run tests
cargo test --workspace --all-features
```

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](./LICENSE) for details.
