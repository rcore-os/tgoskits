
# axdevice_base

[![CI](https://github.com/arceos-hypervisor/axdevice_crates/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/arceos-hypervisor/axdevice_crates/actions/workflows/ci.yml)
[![🚧 Work in Progress](https://img.shields.io/badge/Work_in_Progress-FFD700?style=plastic&logo=github)](https://github.com/arceos-hypervisor/axdevice_crates)

Basic device abstraction library for [AxVisor](https://github.com/arceos-hypervisor/axvisor) virtual device subsystem, designed for `no_std` environments.

## Overview

`axdevice_base` provides core traits, structures, and type definitions for virtual device development, including:

- `BaseDeviceOps` trait: Common interface that all virtual devices must implement.
- `EmulatedDeviceConfig`: Device initialization and configuration structure.
- Device type enumeration `EmuDeviceType` (provided by `axvmconfig` crate).
- Trait aliases for various device types (MMIO, port, system register, etc.).

## Usage Example

```rust,ignore
use axdevice_base::{BaseDeviceOps, EmulatedDeviceConfig, EmuDeviceType};

// Implement a custom device
struct MyDevice { /* ... */ }

impl BaseDeviceOps<axaddrspace::GuestPhysAddrRange> for MyDevice {
    // Implement trait methods ...
}

let config = EmulatedDeviceConfig::default();
```

## Contributing

Issues and PRs are welcome! Please follow the ArceOS-hypervisor project guidelines.

## License

This project is licensed under multiple licenses. You may choose to use this project under any of the following licenses:

- [GPL-3.0-or-later](LICENSE.GPLv3)
- [Apache-2.0](LICENSE.Apache2)
- [MulanPSL2](LICENSE.MulanPSL2)
- [MulanPubL2](LICENSE.MulanPubL2)

You may use this software under the terms of any of these licenses at your option.