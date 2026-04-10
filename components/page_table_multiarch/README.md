<h1 align="center">page_table_multiarch</h1>

<p align="center">Workspace for multi-architecture page table crates</p>

<div align="center">

[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`page_table_multiarch` is a workspace that groups related TGOSKits components under a unified layout. It helps organize closely related crates that are typically developed, versioned, and used together.

> page_table_multiarch was derived from https://github.com/arceos-org/page_table_multiarch

## Workspace Members

- `page_table_multiarch`
- `page_table_entry`

## Quick Start

```bash
# Enter the workspace directory
cd components/page_table_multiarch

# Format code
cargo fmt --all

# Run clippy
cargo clippy --workspace --all-targets --all-features

# Run tests
cargo test --workspace --all-features
```

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](./LICENSE) for details.
