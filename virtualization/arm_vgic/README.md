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
    GicAffinity, GicV3Config, GicV3Controller, GicV3MmioRegion,
    GicV3SpiOwnership, GicV3VcpuWake, GicVcpuId, SoftwareGicV3Backend,
    VgicResult,
};

struct Wake;

impl GicV3VcpuWake for Wake {
    fn wake(&self) -> VgicResult {
        Ok(())
    }
}

fn build() -> VgicResult<(GicV3Controller, arm_vgic::GicV3VcpuBinding)> {
    let config = GicV3Config::new(
        GicV3SpiOwnership::AllGuestOwned,
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

An ITS additionally requires `GicV3Controller::new_with_guest_memory`
and a VM-scoped `GuestMemory` capability. The ITS reads a bounded, checked
command queue and never assumes guest physical addresses equal host addresses.

## SPI ownership and backing

`GicV3SpiOwnership` controls only the guest-visible Distributor resource set:

- `AllGuestOwned` exposes every implemented SPI for a fully virtual machine.
- `Explicit` keeps an SPI RAZ/WI until a software endpoint or a physical
  binding claims it. Mixed register writes cannot modify unclaimed host SPIs.

Backing is selected independently for each endpoint. `configure_spi_input`
claims a software-backed SPI, while `bind_physical_spi` claims an owned host IRQ
and a fixed vCPU affinity. Both kinds may coexist in one controller and one CPU
interface: software events use normal virtual LRs and forwarded physical events
use HW-backed LRs. Hardware forwarding still exits to EL2; it is not a static
CPU/device bypass. A physical binding is electrically driven and therefore
cannot be raised through the software line API.

Binding a physical SPI reserves ownership without modifying host hardware. The
platform backend may snapshot and hand off the released host line when the
guest enables it, and must restore the snapshot on release. Physical MSI
bindings are likewise selected per `(DeviceId, EventId)`; events connected as
software endpoints use the software ITS translation tables.

SGIs and PPIs are always VM-local. `GicV3VcpuBinding::{load, save, synchronize}`
saves the full virtual CPU interface and supports mixed software and HW-backed
entries. LR refill rebuilds a bounded working set instead of pinning every
active interrupt in hardware: pending work is selected before active overflow,
while entries outside the LRs retain their complete pending/active state and
backing. `ICH_HCR_EL2` NPIE, LRENPIE, and UIE request reconciliation when work
remains outside the LRs.

EOImode 0 overflow is reconciled through `ICH_HCR_EL2.EOIcount`. EOImode 1
requires `ICC_DIR_EL1` to be trapped and routed to
`GicV3VcpuBinding::deactivate`; an overflowed HW-backed entry is then
deactivated through the ownership-checked physical backend rather than being
converted into a software interrupt. The platform adapter must verify TDIR
support before constructing the controller, or provide a complete common
CPU-interface system-register trap implementation.

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
