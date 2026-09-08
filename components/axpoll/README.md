<h1 align="center">axpoll</h1>

<p align="center">Scheduler-independent typed I/O readiness contracts</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/axpoll.svg)](https://crates.io/crates/axpoll)
[![Docs.rs](https://docs.rs/axpoll/badge.svg)](https://docs.rs/axpoll)
[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`axpoll` is the pure `no_std` readiness API used by TGOSKits. It defines event
bits, readiness sources, owned registration leases, and typed shared-observer
and exclusive-consumer registration. It deliberately does not own a wait queue,
scheduler, lock implementation, or hard-IRQ wake path.

Use [`axpoll-set`](../axpoll-set) when a task/deferred-context registration queue
with Linux waitqueue selection semantics is required. An OS runtime composes
that queue with its task blocking and IRQ-to-task delivery mechanisms; VFS and
device interfaces continue to depend only on this crate's readiness contracts.

## Quick Start

### Installation

Add this crate to your `Cargo.toml`:

```toml
[dependencies]
axpoll = "0.5.4"
```

### Run Check and Test

```bash
# Enter the crate directory
cd components/axpoll

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

### Example contract

```rust
use axpoll::{IoEvents, Pollable, SharedRegistrationSink};

struct ReadableObject;

impl Pollable for ReadableObject {
    fn poll(&self) -> IoEvents {
        IoEvents::IN
    }

    unsafe fn register_shared(
        &self,
        _sink: &mut dyn SharedRegistrationSink,
        _events: IoEvents,
    ) {
        // A stateful source registers an owned lease in task/deferred context.
    }
}
```

### Documentation

Generate and view API documentation:

```bash
cargo doc --no-deps --open
```

Online documentation: [docs.rs/axpoll](https://docs.rs/axpoll)

# Contributing

1. Fork the repository and create a branch
2. Run local format and checks
3. Run local tests relevant to this crate
4. Submit a PR and ensure CI passes

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](./LICENSE) for details.
