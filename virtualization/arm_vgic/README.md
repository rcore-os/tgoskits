# arm_vgic

`arm_vgic` is a `no_std`, per-VM Arm GICv3 controller model. It models a
Distributor, one Redistributor and virtual CPU interface per vCPU, and an
optional ITS/LPI domain. Platform MMIO mapping, host IRQ discovery, guest
memory, timers, and vCPU scheduling stay outside this crate and enter through
checked capabilities.

Only GICv3 Group 1 Non-secure delivery is supported. GICv2, Secure Group 0/1,
GICv4 vPE, and nested virtualization are intentionally unsupported.

## Construction

Create and validate the whole controller configuration first, then attach every
vCPU before connecting interrupt sources:

```rust
use std::sync::Arc;

use arm_vgic::{
    GicAffinity, GicV3Config, GicV3Controller, GicV3MmioRegion, GicV3Mode,
    GicV3VcpuWake, GicVcpuId, SoftwareGicV3Backend, VgicResult,
};

struct Wake;

impl GicV3VcpuWake for Wake {
    fn wake(&self) -> VgicResult {
        Ok(())
    }
}

fn build() -> VgicResult<(GicV3Controller, arm_vgic::GicV3VcpuBinding)> {
    let config = GicV3Config::new(
        GicV3Mode::Emulated,
        GicV3MmioRegion::new(0x0800_0000, 0x1_0000)?,
        GicV3MmioRegion::new(0x080a_0000, 0x2_0000)?,
        0x2_0000,
        1,
    )?;
    let controller = GicV3Controller::new(config, Arc::new(SoftwareGicV3Backend))?;
    let binding = controller.attach_vcpu(
        GicVcpuId::new(0),
        GicAffinity::new(0, 0, 0, 0),
        Arc::new(Wake),
    )?;
    Ok((controller, binding))
}
```

Keep each binding alive for as long as its vCPU is attached. Dropping a binding
releases that Redistributor so failed VM construction and teardown can roll back
without leaving controller state behind.

An emulated ITS additionally requires `GicV3Controller::new_with_guest_memory`
and a VM-scoped `GuestMemory` capability. The ITS reads a bounded, checked
command queue and never assumes guest physical addresses equal host addresses.

## Modes

- `Emulated` keeps GICD, GICR, SGI/PPI/SPI/LPI, CPU-interface, and ITS state per
  VM. `GicV3VcpuBinding::{load, save, synchronize}` saves all configured list
  registers and refills software-pending work.
- `Passthrough` requires explicit guest SPI/ITS ownership through
  `bind_physical_spi` and `bind_physical_msi`. Delivery goes only through the
  supplied physical backend; it never falls back to virtual list registers.
  Binding an SPI leaves the physical line masked. Guest Distributor enable
  state is applied only while the fixed target vCPU binding is loaded, and the
  line is masked again when that binding is saved. Guest accesses to a shared
  host ITS frame are rejected.

Backends must validate physical IRQ identity, target affinity, address ranges,
access widths, and resource ownership. Backend callbacks are issued after the
controller state lock is released.

## Errors

All fallible APIs return `VgicResult<T>` with a matchable `VgicError`. Errors
distinguish invalid INTIDs, wrong INTID classes, invalid register accesses,
state transitions, guest-memory failures, malformed or over-budget ITS
commands, missing/conflicting resources, unsupported operations, and backend
failures. Unknown architecturally RAZ/WI registers return zero or ignore writes;
invalid width, alignment, range, and ownership are explicit errors.

## API compatibility

The GICv3 API replaces the former `Vgic`/GICv2 types, global host callback,
global ITS/LPI state, timer devices, and manual hardware-injection functions.
Integrators must register a `GicV3Controller`, attach vCPUs, and connect devices
through controller-owned wired or MSI endpoints. Virtual timer integration now
belongs to the VMM and should connect a per-vCPU PPI line.

## Validation

```bash
cargo fmt --all --check
cargo clippy -p arm_vgic --all-targets --all-features -- -D warnings
cargo test -p arm_vgic --all-features
RUSTDOCFLAGS="-D warnings" cargo doc -p arm_vgic --all-features --no-deps
```

Licensed under Apache-2.0.
