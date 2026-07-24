# AxVirtIO Devices

[![CI](https://github.com/arceos-hypervisor/axvirtio-devices/actions/workflows/ci.yml/badge.svg)](https://github.com/arceos-hypervisor/axvirtio-devices/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-GPL--3.0%20OR%20Apache--2.0%20OR%20MulanPSL--2.0-blue.svg)](LICENSE)

A collection of VirtIO device implementations for the [ArceOS-Hypervisor](https://github.com/arceos-hypervisor/) project. This workspace provides `no_std` compatible VirtIO device emulation following the [VirtIO 1.1 specification](https://docs.oasis-open.org/virtio/virtio/v1.1/virtio-v1.1.html).

## Overview

AxVirtIO Devices provides modular, reusable VirtIO device implementations designed for hypervisor and embedded systems development. The project is organized as a Rust workspace with:

- **`axvirtio-common`**: Common types, traits, and utilities shared across all VirtIO device implementations
- **`axvirtio-blk`**: VirtIO block device implementation with MMIO transport

## Features

- 🚀 **`no_std` Compatible**: Designed for bare-metal and embedded environments
- 📦 **Modular Design**: Clean separation between common infrastructure and device-specific code
- 🔌 **Pluggable Backends**: Abstract backend traits allow flexible storage implementations
- 🔒 **Thread Safe**: Proper synchronization primitives for concurrent access
- ✅ **Well Tested**: Comprehensive test suite for all components
- 📚 **Documented**: Detailed documentation with examples

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                      Guest VM                                │
│  ┌─────────────────────────────────────────────────────┐    │
│  │                  Guest Driver                        │    │
│  │            (VirtIO Block Driver)                     │    │
│  └──────────────────────┬──────────────────────────────┘    │
└─────────────────────────┼───────────────────────────────────┘
                          │ MMIO Access
┌─────────────────────────┼───────────────────────────────────┐
│                         ▼                                    │
│  ┌─────────────────────────────────────────────────────┐    │
│  │              VirtIO MMIO Transport                   │    │
│  │         (axvirtio-common/mmio)                       │    │
│  └──────────────────────┬──────────────────────────────┘    │
│                         │                                    │
│  ┌─────────────────────────────────────────────────────┐    │
│  │               VirtIO Queue Layer                     │    │
│  │    (Descriptor Table, Available Ring, Used Ring)     │    │
│  │         (axvirtio-common/queue)                      │    │
│  └──────────────────────┬──────────────────────────────┘    │
│                         │                                    │
│  ┌─────────────────────────────────────────────────────┐    │
│  │            VirtIO Block Device                       │    │
│  │              (axvirtio-blk)                          │    │
│  └──────────────────────┬──────────────────────────────┘    │
│                         │                                    │
│  ┌─────────────────────────────────────────────────────┐    │
│  │              Block Backend Trait                     │    │
│  │           (Pluggable Storage)                        │    │
│  └─────────────────────────────────────────────────────┘    │
│                      Hypervisor                              │
└─────────────────────────────────────────────────────────────┘
```

## Crates

### axvirtio-common

Common infrastructure for VirtIO device implementations:

- **Queue Management**: Complete VirtIO queue implementation including descriptor tables, available rings, and used rings
- **MMIO Transport**: Memory-mapped I/O transport layer utilities
- **Configuration**: Device configuration structures and constants
- **Error Handling**: Unified error types for VirtIO operations

### axvirtio-blk

VirtIO block device implementation:

- **MMIO Block Device**: Full VirtIO MMIO block device following the specification
- **Request Handling**: Support for read, write, and flush operations
- **Pluggable Backend**: `BlockBackend` trait for custom storage implementations
- **Guest Memory Access**: Abstract guest memory accessor for address translation

## Usage

### Adding Dependencies

Add the following to your `Cargo.toml`:

```toml
[dependencies]
axvirtio-blk = { git = "https://github.com/arceos-hypervisor/axvirtio-devices" }
axvirtio-common = { git = "https://github.com/arceos-hypervisor/axvirtio-devices" }
```

### Creating a VirtIO Block Device

```rust
use axvirtio_blk::{VirtioMmioBlockDevice, BlockBackend, VirtioBlockConfig, VirtioResult};
use axaddrspace::{GuestMemoryAccessor, GuestPhysAddr};
use memory_addr::PhysAddr;

// 1. Implement your block backend
struct MyBlockBackend {
    // Your storage implementation
}

impl BlockBackend for MyBlockBackend {
    fn read(&self, sector: u64, buffer: &mut [u8]) -> VirtioResult<usize> {
        // Read sectors from your storage
        Ok(buffer.len())
    }

    fn write(&self, sector: u64, buffer: &[u8]) -> VirtioResult<usize> {
        // Write sectors to your storage
        Ok(buffer.len())
    }

    fn flush(&self) -> VirtioResult<()> {
        // Flush pending writes
        Ok(())
    }
}

// 2. Implement guest memory accessor for address translation
#[derive(Clone)]
struct MyMemoryAccessor {
    // Your memory translation implementation
}

impl GuestMemoryAccessor for MyMemoryAccessor {
    fn translate_and_get_limit(&self, guest_addr: GuestPhysAddr) -> Option<(PhysAddr, usize)> {
        // Translate guest physical address to host physical address
        // Return (host_addr, accessible_size)
        None
    }
}

// 3. Create the VirtIO block device
fn create_device() {
    let backend = MyBlockBackend { /* ... */ };
    let accessor = MyMemoryAccessor { /* ... */ };
    let config = VirtioBlockConfig::default();
    let base_addr = GuestPhysAddr::from(0x0a000000);

    let device = VirtioMmioBlockDevice::new(
        base_addr,
        0x200,  // MMIO region size
        backend,
        config,
        accessor,
    ).unwrap();

    // Handle MMIO accesses from the guest
    // device.mmio_read(addr, width)
    // device.mmio_write(addr, width, value)
}
```

### VirtIO Block Configuration

```rust
use axvirtio_blk::VirtioBlockConfig;

let config = VirtioBlockConfig {
    capacity: 2048 * 1024,  // Total sectors (1GB with 512-byte sectors)
    size_max: 65536,        // Maximum segment size
    seg_max: 128,           // Maximum number of segments
    blk_size: 512,          // Block size in bytes
    // ... other configuration options
    ..Default::default()
};
```

## Supported Features

### VirtIO Features

| Feature | Status | Description |
|---------|--------|-------------|
| `VIRTIO_F_VERSION_1` | ✅ | VirtIO 1.0+ compliance |
| `VIRTIO_F_RING_EVENT_IDX` | ✅ | Event index support |
| `VIRTIO_BLK_F_SIZE_MAX` | ✅ | Maximum segment size |
| `VIRTIO_BLK_F_SEG_MAX` | ✅ | Maximum segments per request |
| `VIRTIO_BLK_F_BLK_SIZE` | ✅ | Block size reporting |
| `VIRTIO_BLK_F_FLUSH` | ✅ | Flush command support |

### Block Operations

| Operation | Status | Description |
|-----------|--------|-------------|
| Read | ✅ | Read sectors from device |
| Write | ✅ | Write sectors to device |
| Flush | ✅ | Flush pending writes |

## Testing

Run the test suite:

```bash
# Run all tests
cargo test

# Run tests for a specific crate
cargo test --package axvirtio-blk

# Run tests with output
cargo test -- --nocapture
```

## Supported Targets

The library supports the following targets:

- `x86_64-unknown-linux-gnu` (with tests)
- `x86_64-unknown-none`
- `riscv64gc-unknown-none-elf`
- `aarch64-unknown-none-softfloat`

## Documentation

Generate and view documentation:

```bash
cargo doc --open
```

## Contributing

Contributions are welcome! Please feel free to submit issues and pull requests.

## License

This project is licensed under one of the following licenses:

- GNU General Public License v3.0 or later ([GPL-3.0-or-later](LICENSE-GPL))
- Apache License, Version 2.0 ([Apache-2.0](LICENSE-APACHE))
- Mulan Permissive Software License, Version 2 ([MulanPSL-2.0](LICENSE-MULAN))

## Related Projects

- [ArceOS-Hypervisor](https://github.com/arceos-hypervisor/axvisor) - The hypervisor framework using this library
- [axaddrspace](https://github.com/arceos-hypervisor/axaddrspace) - Guest address space management

## References

- [VirtIO Specification 1.1](https://docs.oasis-open.org/virtio/virtio/v1.1/virtio-v1.1.html)
- [VirtIO Block Device Specification](https://docs.oasis-open.org/virtio/virtio/v1.1/cs01/virtio-v1.1-cs01.html#x1-2390002)
