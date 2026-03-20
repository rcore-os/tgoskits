<h1 align="center">axdevice</h1>

<p align="center">OS-Agnostic Virtual Device Abstraction Layer</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/axdevice.svg)](https://crates.io/crates/axdevice)
[![Docs.rs](https://docs.rs/axdevice/badge.svg)](https://docs.rs/axdevice)
[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://github.com/arceos-hypervisor/axdevice/blob/main/LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`axdevice` is a reusable, OS-agnostic device abstraction layer for virtual machines. It provides unified management for emulated devices and dispatches guest accesses to MMIO, system-register, and port-based devices in `#![no_std]` environments.

This crate currently exports two core types:

- **`AxVmDeviceConfig`** - Wraps a list of `EmulatedDeviceConfig` items used to initialize VM devices
- **`AxVmDevices`** - Manages device collections, dispatches device access requests, and allocates IVC channels

The crate is suitable for hypervisors and low-level OS components targeting AArch64 or RISC-V.

## Quick Start

### Requirements

- Rust nightly toolchain
- Rust components: rust-src, clippy, rustfmt

```bash
# Install rustup (if not installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install nightly toolchain and components
rustup install nightly
rustup component add rust-src clippy rustfmt --toolchain nightly
```

### Run Check and Test

```bash
# 1. Enter the repository
cd axdevice

# 2. Code check (format + clippy + build)
./scripts/check.sh

# 3. Run tests
./scripts/test.sh
```

## Integration

### Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
axdevice = "0.2.2"
```

### Example

```rust
use std::sync::{Arc, Mutex};

use axaddrspace::device::AccessWidth;
use axaddrspace::{GuestPhysAddr, GuestPhysAddrRange};
use axdevice::{AxVmDeviceConfig, AxVmDevices};
use axdevice_base::BaseDeviceOps;
use axerrno::AxResult;
use axvmconfig::EmulatedDeviceType;

struct MockMmioDevice {
    range: GuestPhysAddrRange,
    last_write: Mutex<Option<usize>>,
}

impl MockMmioDevice {
    fn new(base: usize, size: usize) -> Self {
        Self {
            range: GuestPhysAddrRange::new(
                GuestPhysAddr::from(base),
                GuestPhysAddr::from(base + size),
            ),
            last_write: Mutex::new(None),
        }
    }
}

impl BaseDeviceOps<GuestPhysAddrRange> for MockMmioDevice {
    fn address_range(&self) -> GuestPhysAddrRange {
        self.range
    }

    fn emu_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::IVCChannel
    }

    fn handle_read(&self, _addr: GuestPhysAddr, _width: AccessWidth) -> AxResult<usize> {
        Ok(0xDEAD_BEEF)
    }

    fn handle_write(&self, addr: GuestPhysAddr, _width: AccessWidth, val: usize) -> AxResult {
        let offset = addr.as_usize() - self.range.start.as_usize();
        assert_eq!(offset, 0x40);
        *self.last_write.lock().unwrap() = Some(val);
        Ok(())
    }
}

fn main() {
    let config = AxVmDeviceConfig::new(vec![]);
    let mut devices = AxVmDevices::new(config);

    let mock = Arc::new(MockMmioDevice::new(0x1000_0000, 0x1000));
    devices.add_mmio_dev(mock.clone());

    let width = AccessWidth::try_from(4).unwrap();
    let addr = GuestPhysAddr::from(0x1000_0040);

    devices.handle_mmio_write(addr, width, 0x1234_5678).unwrap();
    let value = devices.handle_mmio_read(addr, width).unwrap();

    assert_eq!(value, 0xDEAD_BEEF);
    assert_eq!(*mock.last_write.lock().unwrap(), Some(0x1234_5678));
}
```

### Documentation

Generate and view API documentation:

```bash
cargo doc --no-deps --open
```

Online documentation: [docs.rs/axdevice](https://docs.rs/axdevice)

# Contributing

1. Fork the repository and create a branch
2. Run local check: `./scripts/check.sh`
3. Run local tests: `./scripts/test.sh`
4. Submit PR and pass CI checks

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) for details.
