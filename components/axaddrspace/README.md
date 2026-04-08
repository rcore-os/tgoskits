<h1 align="center">axaddrspace</h1>

<p align="center">ArceOS-Hypervisor guest VM address space management module</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/axaddrspace.svg)](https://crates.io/crates/axaddrspace)
[![Docs.rs](https://docs.rs/axaddrspace/badge.svg)](https://docs.rs/axaddrspace)
[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://github.com/arceos-hypervisor/axaddrspace/blob/main/LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`axaddrspace` is the guest address space management crate for the
[ArceOS-Hypervisor](https://github.com/arceos-hypervisor/) project. It provides
nested page table management, guest physical address translation, memory
mapping backends, and nested page fault handling for hypervisor environments.

This crate supports multiple architectures:

- **x86_64** - VMX Extended Page Tables (EPT)
- **AArch64** - Stage-2 page tables
- **RISC-V** - Nested page tables based on the hypervisor extension

Key capabilities include:

- **`AddrSpace`** - address space creation, mapping, unmapping, and translation
- **`AxMmHal`** - hardware abstraction trait for frame allocation and address conversion
- **Linear mapping backend** - map known contiguous host physical memory ranges
- **Allocation mapping backend** - allocate frames eagerly or lazily on page faults
- **Guest memory helpers** - translate guest addresses to accessible host buffers

Supports `#![no_std]` and is intended for bare-metal hypervisor and kernel use.

## Quick Start

### Requirements

- Rust nightly toolchain
- Rust components: `rust-src`, `clippy`, `rustfmt`

```bash
# Install rustup (if not installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install nightly toolchain and components
rustup install nightly
rustup component add rust-src clippy rustfmt --toolchain nightly
```

### Run Check and Test

```bash
# 1. Clone the repository
git clone https://github.com/arceos-hypervisor/axaddrspace.git
cd axaddrspace

# 2. Code check
./scripts/check.sh

# 3. Run tests
./scripts/test.sh

# 4. Run a specific integration test target directly
cargo test --test address_space
```

The helper scripts download the shared `axci` test/check framework on first run.

## Integration

### Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
axaddrspace = "0.3.0"
```

### Example

```rust
use axaddrspace::{AddrSpace, AxMmHal, GuestPhysAddr, HostPhysAddr, HostVirtAddr, MappingFlags};
use memory_addr::{PhysAddr, VirtAddr};
use ax_page_table_multiarch::PagingHandler;

struct MyHal;

impl AxMmHal for MyHal {
    fn alloc_frame() -> Option<HostPhysAddr> {
        unimplemented!()
    }

    fn dealloc_frame(_paddr: HostPhysAddr) {
        unimplemented!()
    }

    fn phys_to_virt(_paddr: HostPhysAddr) -> HostVirtAddr {
        unimplemented!()
    }

    fn virt_to_phys(_vaddr: HostVirtAddr) -> HostPhysAddr {
        unimplemented!()
    }
}

impl PagingHandler for MyHal {
    fn alloc_frame() -> Option<PhysAddr> {
        <Self as AxMmHal>::alloc_frame()
    }

    fn dealloc_frame(paddr: PhysAddr) {
        <Self as AxMmHal>::dealloc_frame(paddr)
    }

    fn phys_to_virt(paddr: PhysAddr) -> VirtAddr {
        <Self as AxMmHal>::phys_to_virt(paddr)
    }
}

fn example() -> ax_errno::AxResult<()> {
    let base = GuestPhysAddr::from_usize(0x1000_0000);
    let mut addr_space = AddrSpace::<MyHal>::new_empty(4, base, 0x20_0000)?;

    addr_space.map_linear(
        base,
        PhysAddr::from_usize(0x8000_0000),
        0x10_0000,
        MappingFlags::READ | MappingFlags::WRITE,
    )?;

    addr_space.map_alloc(
        base + 0x10_0000,
        0x2000,
        MappingFlags::READ | MappingFlags::WRITE,
        false,
    )?;

    let fault_handled = addr_space.handle_page_fault(
        base + 0x10_0000,
        MappingFlags::READ,
    );
    assert!(fault_handled);

    let host_paddr = addr_space.translate(base).unwrap();
    assert_eq!(host_paddr, PhysAddr::from_usize(0x8000_0000));

    Ok(())
}
```

### Features

- `arm-el2`: enable AArch64 EL2 support
- `default`: includes `arm-el2`

### Documentation

Generate and view API documentation:

```bash
cargo doc --no-deps --open
```

Online documentation: [docs.rs/axaddrspace](https://docs.rs/axaddrspace)

# Contributing

1. Fork the repository and create a branch
2. Run local check: `./scripts/check.sh`
3. Run local tests: `./scripts/test.sh`
4. Submit PR and pass CI checks

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) for details.
