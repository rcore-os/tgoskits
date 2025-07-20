# axaddrspace

**ArceOS-Hypervisor guest VM address space management module**

[![CI](https://github.com/arceos-hypervisor/axaddrspace/actions/workflows/ci.yml/badge.svg)](https://github.com/arceos-hypervisor/axaddrspace/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/axaddrspace)](https://crates.io/crates/axaddrspace)
[![License](https://img.shields.io/badge/license-Apache%202.0%20OR%20MIT-blue)](LICENSE)

## Overview

`axaddrspace` is a core component of the [ArceOS-Hypervisor](https://github.com/arceos-hypervisor/) project that provides guest virtual machine address space management capabilities. The crate implements nested page tables and address translation for hypervisor environments, supporting multiple architectures including x86_64, AArch64, and RISC-V.

## Features

- **Multi-architecture support**: x86_64 (VMX EPT), AArch64 (Stage 2 page tables), and RISC-V nested page tables
- **Flexible memory mapping backends**:
  - **Linear mapping**: For contiguous physical memory regions with known addresses
  - **Allocation mapping**: Dynamic allocation with optional lazy loading support
- **Nested page fault handling**: Comprehensive page fault management for guest VMs
- **Hardware abstraction layer**: Clean interface for memory management operations
- **No-std compatible**: Designed for bare-metal hypervisor environments

## Architecture Support

### x86_64
- VMX Extended Page Tables (EPT)
- Memory type configuration (WriteBack, Uncached, etc.)
- Execute permissions for user-mode addresses

### AArch64
- VMSAv8-64 Stage 2 translation tables
- Configurable MAIR_EL2 memory attributes
- EL2 privilege level support

### RISC-V
- Nested page table implementation
- Hypervisor fence instructions (`hfence.vvma`)
- Sv39 metadata support

## Core Components

### Address Space Management
The `AddrSpace` struct provides:
- Virtual address range management
- Page table root address tracking
- Memory area organization
- Address translation services

### Memory Mapping Backends
Two types of mapping backends are supported:

1. **Linear Backend**: Direct mapping with constant offset between virtual and physical addresses
2. **Allocation Backend**: Dynamic memory allocation with optional population strategies

### Nested Page Tables
Architecture-specific nested page table implementations:
- **x86_64**: `ExtendedPageTable` with EPT entries
- **AArch64**: Stage 2 page tables with descriptor attributes
- **RISC-V**: Sv39-based nested page tables

## Usage

Add this to your `Cargo.toml`:

```toml
[dependencies]
axaddrspace = "0.1.0"
```

### Basic Example

```rust
use axaddrspace::{AddrSpace, MappingFlags, GuestPhysAddr};
use page_table_multiarch::PagingHandler;

// Create a new address space
let mut addr_space = AddrSpace::<YourPagingHandler>::new_empty(
    GuestPhysAddr::from(0x1000_0000),
    0x1000_0000, // 256MB
)?;

// Create a linear mapping
addr_space.map_linear(
    GuestPhysAddr::from(0x1000_0000), // Guest virtual address
    PhysAddr::from(0x8000_0000),      // Host physical address
    0x10_0000,                        // 1MB
    MappingFlags::READ | MappingFlags::WRITE,
)?;

// Handle a nested page fault
let fault_handled = addr_space.handle_page_fault(
    GuestPhysAddr::from(0x1000_1000),
    MappingFlags::READ,
);
```

### Hardware Abstraction Layer

Implement the `AxMmHal` trait for your platform:

```rust
use axaddrspace::{AxMmHal, HostPhysAddr, HostVirtAddr};

struct MyHal;

impl AxMmHal for MyHal {
    fn alloc_frame() -> Option<HostPhysAddr> {
        // Your frame allocation implementation
    }

    fn dealloc_frame(paddr: HostPhysAddr) {
        // Your frame deallocation implementation  
    }

    fn phys_to_virt(paddr: HostPhysAddr) -> HostVirtAddr {
        // Your physical to virtual address conversion
    }

    fn virt_to_phys(vaddr: HostVirtAddr) -> HostPhysAddr {
        // Your virtual to physical address conversion
    }
}
```

## Configuration

### Feature Flags

- `arm-el2`: Enable AArch64 EL2 support (default)
- `default`: Includes `arm-el2` feature

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

## Repository

- [GitHub Repository](https://github.com/arceos-hypervisor/axaddrspace)
- [ArceOS-Hypervisor Project](https://github.com/arceos-hypervisor/)
