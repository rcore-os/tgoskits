# axhvc

[![Crates.io](https://img.shields.io/crates/v/axhvc)](https://crates.io/crates/axhvc)
[![Docs.rs](https://docs.rs/axhvc/badge.svg)](https://docs.rs/axhvc)
[![CI](https://github.com/arceos-hypervisor/axhvc/actions/workflows/ci.yml/badge.svg)](https://github.com/arceos-hypervisor/axhvc/actions/workflows/ci.yml)

AxVisor HyperCall definitions for guest-hypervisor communication.

## Overview

This crate provides the hypercall interface for [AxVisor](https://github.com/arceos-hypervisor/axvisor), a type-1 hypervisor based on [ArceOS](https://github.com/arceos-org/arceos). It defines the hypercall codes and result types used for communication between guest VMs and the hypervisor.

## Features

- `no_std` compatible - suitable for bare-metal and embedded environments
- Defines all supported hypercall operations
- Provides type-safe hypercall codes with numeric enum conversion
- Cross-platform support (x86_64, RISC-V, AArch64)

## Supported Hypercalls

| Code | Name | Description |
|------|------|-------------|
| 0 | `HypervisorDisable` | Disable the hypervisor |
| 1 | `HyperVisorPrepareDisable` | Prepare to disable the hypervisor |
| 2 | `HyperVisorDebug` | Debug hypercall (development only) |
| 3 | `HIVCPublishChannel` | Publish an IVC shared memory channel |
| 4 | `HIVCSubscribChannel` | Subscribe to an IVC shared memory channel |
| 5 | `HIVCUnPublishChannel` | Unpublish an IVC shared memory channel |
| 6 | `HIVCUnSubscribChannel` | Unsubscribe from an IVC shared memory channel |

## Usage

Add this to your `Cargo.toml`:

```toml
[dependencies]
axhvc = "0.1"
```

### Example

```rust,ignore
use axhvc::{HyperCallCode, HyperCallResult};

fn handle_hypercall(code: HyperCallCode) -> HyperCallResult {
    match code {
        HyperCallCode::HypervisorDisable => {
            // Handle hypervisor disable request
            Ok(0)
        }
        HyperCallCode::HIVCPublishChannel => {
            // Handle IVC channel publish request
            Ok(0)
        }
        _ => Err(axerrno::AxError::Unsupported),
    }
}
```

## License

Licensed under one of the following licenses:

- GNU General Public License, Version 3.0 or later ([LICENSE-GPL](LICENSE-GPL) or https://www.gnu.org/licenses/gpl-3.0.html)
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- Mulan PSL v2 ([LICENSE-MULAN](LICENSE-MULAN) or https://license.coscl.org.cn/MulanPSL2)
