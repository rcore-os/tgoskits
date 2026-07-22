<h1 align="center">riscv_vplic</h1>

<p align="center">VM-local, software RISC-V PLIC emulation</p>

English | [中文](README_CN.md)

`riscv_vplic` is a `no_std + alloc` PLIC 1.0.0 device model for hypervisors. Each `VPlicGlobal` owns one VM's priority, enable, pending, active, source-level, threshold, and claim/complete state.

The crate never dereferences a host PLIC aperture and does not assume that host and guest PLIC addresses are identical. A platform IRQ adapter owns physical IRQ routing and signals assigned vPLIC inputs through the AxVM interrupt topology.

## Features

- PLIC priority, pending, enable, threshold, and claim/complete registers
- Independent state for every VM and context
- Edge and level input APIs, including level re-pending after completion
- Explicit per-VM source ownership with RAZ/WI for unassigned sources
- Typed construction, MMIO, context, and source errors

## Usage

```rust
use axvm_types::GuestPhysAddr;
use riscv_vplic::VPlicGlobal;

let plic = VPlicGlobal::new(
    GuestPhysAddr::from(0x0c00_0000),
    Some(0x40_0000),
    2,
)?;
plic.restrict_to_assigned_sources();
plic.assign_source(10)?;
plic.set_source_level(10, true)?;
# Ok::<(), riscv_vplic::VplicError>(())
```

AxVM always installs an explicit source assignment policy from `VmMachinePlan`. Physical IRQ ownership and host claim/complete belong to the host adapter, not this crate.

## Validation

```bash
cargo fmt --all
cargo xtask clippy --package riscv_vplic
cargo test -p riscv_vplic --all-features
RUSTDOCFLAGS="-Dwarnings" cargo doc -p riscv_vplic --no-deps
```

Licensed under Apache-2.0.
