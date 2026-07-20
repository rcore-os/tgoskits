<h1 align="center">axvm</h1>

<p align="center">Virtual Machine resource management crate for ArceOS's hypervisor variant</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/axvm.svg)](https://crates.io/crates/axvm)
[![Docs.rs](https://docs.rs/axvm/badge.svg)](https://docs.rs/axvm)
[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`axvm` owns VM machine planning and transactional resource lifecycle. A strict
`VmMachineRequest` plus a host FDT/ACPI snapshot becomes an immutable
`VmMachinePlan` containing memory, mappings, devices, interrupt topology, and
guest firmware resources. VM preparation commits the whole plan or rolls it
back.

Passthrough machines derive authorized hardware from the host snapshot;
Virtual machines map no host I/O and allocate a new virtual platform. The crate
uses rust-vmm `vm-allocator`, `vm-fdt`, and `acpi_tables`, and keeps a
`no_std + alloc` runtime with an optional `std` test feature.

## Quick Start

### Installation

Add this crate to your `Cargo.toml`:

```toml
[dependencies]
axvm = "0.5.0"
```

### Run Check and Test

```bash
# Enter the crate directory
cd virtualization/axvm

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
use axvm::machine::{HostPlatformSnapshot, VmMachinePlanner};

let plan = VmMachinePlanner::new(architecture_profile)
    .plan(&machine_request, &HostPlatformSnapshot::new(0))?;
```

The build order is RAM, vCPUs, interrupt controllers and bindings, devices and
topology, bus mappings, firmware, boot state, and commit. Physical-device
leases restore ownership on every failure path.

Firmware nodes with `status = "disabled"` are recorded as inactive aliases.
They neither claim a device nor authorize an I/O aperture, and they do not
hide an overlapping resource that an active assigned node owns. Consequently,
an inactive-only range remains unmapped while common alternative bindings for
one physical device do not punch a hole in an authorized passthrough mapping.

`HostPlatformSnapshot` records the firmware-selected console independently
from its UART model. This identity lets mediated guests replace the correct
host node even when several compatible UARTs exist. It is not authorization by
itself: a live platform adapter must classify the device as transferable, and
the VM host must retain a reversible console-output lease while a direct guest
owns the physical UART.

### Documentation

Generate and view API documentation:

```bash
cargo doc --no-deps --open
```

Online documentation: [docs.rs/axvm](https://docs.rs/axvm)

# Contributing

1. Fork the repository and create a branch
2. Run local format and checks
3. Run local tests relevant to this crate
4. Submit a PR and ensure CI passes

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](./LICENSE) for details.
