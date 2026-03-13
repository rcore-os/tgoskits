# axdevice_base

[![CI](https://github.com/arceos-hypervisor/axdevice_base/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/arceos-hypervisor/axdevice_base/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/axdevice_base)](https://crates.io/crates/axdevice_base)
[![Docs.rs](https://docs.rs/axdevice_base/badge.svg)](https://docs.rs/axdevice_base)

Basic device abstraction library for [AxVisor](https://github.com/arceos-hypervisor/axvisor) virtual device subsystem, designed for `no_std` environments.

## Overview

`axdevice_base` provides core traits, structures, and type definitions for virtual device development, including:

- `BaseDeviceOps` trait: Common interface that all virtual devices must implement.
- `EmulatedDeviceConfig`: Device initialization and configuration structure.
- Device type enumeration `EmuDeviceType` (re-exported from `axvmconfig` crate).
- Trait aliases for various device types:
  - `BaseMmioDeviceOps`: For MMIO (Memory-Mapped I/O) devices.
  - `BaseSysRegDeviceOps`: For system register devices (ARM).
  - `BasePortDeviceOps`: For port I/O devices (x86).
- `map_device_of_type`: Helper function for runtime device type checking and casting.

## Features

- **`no_std` compatible**: Designed for bare-metal and hypervisor environments.
- **Multi-architecture support**: x86_64, AArch64, RISC-V64.
- **Type-safe addressing**: Different address range types for different device access methods.
- **Serialization support**: Device configuration can be serialized/deserialized via `serde`.

## Usage

Add this to your `Cargo.toml`:

```toml
[dependencies]
axdevice_base = "0.1"
```

### Implementing a Custom Device

```rust,ignore
use axdevice_base::{BaseDeviceOps, EmuDeviceType};
use axaddrspace::{GuestPhysAddr, GuestPhysAddrRange, device::AccessWidth};
use axerrno::AxResult;

struct MyUartDevice {
    base_addr: usize,
    // ... device state
}

impl BaseDeviceOps<GuestPhysAddrRange> for MyUartDevice {
    fn emu_type(&self) -> EmuDeviceType {
        EmuDeviceType::Dummy  // Use appropriate device type
    }

    fn address_range(&self) -> GuestPhysAddrRange {
        (self.base_addr..self.base_addr + 0x1000).try_into().unwrap()
    }

    fn handle_read(&self, addr: GuestPhysAddr, width: AccessWidth) -> AxResult<usize> {
        // Handle read operation from guest
        Ok(0)
    }

    fn handle_write(&self, addr: GuestPhysAddr, width: AccessWidth, val: usize) -> AxResult {
        // Handle write operation from guest
        Ok(())
    }
}
```

### Device Configuration

```rust
use axdevice_base::EmulatedDeviceConfig;

let config = EmulatedDeviceConfig {
    name: "uart0".into(),
    base_ipa: 0x0900_0000,
    length: 0x1000,
    irq_id: 33,
    emu_type: 1,
    cfg_list: vec![115200],  // device-specific config (e.g., baud rate)
};
```

### Type Checking with `map_device_of_type`

```rust,ignore
use axdevice_base::{BaseMmioDeviceOps, map_device_of_type};
use alloc::sync::Arc;

fn process_device(device: &Arc<dyn BaseMmioDeviceOps>) {
    // Try to access device-specific methods if it's a UartDevice
    if let Some(baud_rate) = map_device_of_type(device, |uart: &MyUartDevice| {
        uart.get_baud_rate()
    }) {
        println!("UART baud rate: {}", baud_rate);
    }
}
```

## Supported Platforms

| Architecture | MMIO Devices | Port I/O Devices | System Register Devices |
|--------------|--------------|------------------|-------------------------|
| x86_64       | ✓            | ✓                | -                       |
| AArch64      | ✓            | -                | ✓                       |
| RISC-V64     | ✓            | -                | -                       |

## Documentation

For detailed API documentation, visit [docs.rs/axdevice_base](https://docs.rs/axdevice_base).

## Contributing

Issues and PRs are welcome! Please follow the [ArceOS-hypervisor project guidelines](https://github.com/arceos-hypervisor/axvisor).

## License

Axdevice_base is licensed under the Apache License, Version 2.0. See the [LICENSE](./LICENSE) file for details.