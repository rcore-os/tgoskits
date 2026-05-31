<h1 align="center">axvisor_api</h1>

<p align="center">Workspace for AxVisor API related crates</p>

<div align="center">

[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`axvisor_api` is the host runtime abstraction layer used by AxVisor. It collects
the host-facing capabilities that the hypervisor core depends on, so lower
layers can call stable interfaces instead of directly depending on ArceOS
runtime internals.

> axvisor_api was derived from https://github.com/arceos-org/axvisor_api

## Current Runtime Modules

The current API surface is organized by capability:

- `host`: CPU enumeration and host task/thread helpers
- `task`: host task handles, wait queues, and vCPU task spawning
- `memory`: frame allocation and address translation
- `time`: monotonic time, timer registration, and one-shot timer programming
- `irq`: host IRQ dispatch and hook/handler registration
- `platform`: boot firmware discovery and host resource handoff
- `fs`: file, directory, cwd, and stdio helpers
- `process`: process termination helpers
- `vmm`: VM/vCPU context and interrupt injection helpers
- `arch`: architecture-specific virtualization hooks
- `console`: host console I/O

## Workspace Members

- `axvisor_api_proc`

## Quick Start

```bash
# Enter the workspace directory
cd virtualization/axvisor_api

# Format code
cargo fmt --all

# Run clippy
cargo clippy --workspace --all-targets --all-features

# Run tests
cargo test --workspace --all-features
```

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](./LICENSE) for details.
