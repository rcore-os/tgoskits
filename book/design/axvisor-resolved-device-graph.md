# Axvisor resolved device graph and guest firmware

Status: implementation baseline for PR #1718

## Problem and goals

Axvisor previously described a device in several places: machine defaults,
runtime factories, bus registrations, FDT/ACPI builders, and architecture boot
code. A fixed address or IRQ could consequently be accepted by one layer and
conflict in another. Adding a virtual device also required the caller to know
platform allocation policy.

The resolved device graph makes one architecture-owned declaration the source
of truth before guest firmware is finalized. A new purely virtual device
factory declares named MMIO, PIO, wired IRQ, or MSI slots. Architecture pools
then assign deterministic values; the factory consumes those values during
construction, and firmware uses the same immutable result.

This mechanism does not unify architecture initialization policy. AArch64,
RISC-V, x86, and LoongArch retain their controller topology and boot order.

## Dependency and ownership boundaries

```text
machine profile + normalized host firmware
                 |
       architecture DeviceGraphBuilder
                 |
          declare every factory
                 |
      deterministic resource planner
                 |
         ResolvedDeviceGraph
           /             \
 guest FDT or ACPI     DeviceRuntime builder
                           |
                 ResourceLease + seal
```

`axdevice_base` owns typed interrupt identifiers, electrical IRQ-line
semantics, and narrow controller capabilities. `axdevice` owns the graph,
resource planner, claim lifecycle, factories, bundles, and runtime indices.
`axvm` owns architecture plans, firmware construction, memory mapping, vCPU
binding, and architecture-specific device order.

Interrupt controller state remains in the concrete controller. The graph and
runtime do not duplicate pending, active, routing, EOI, host IRQ, or LR state.

## Graph nodes

Every node has a stable `DeviceNodeId`, optional firmware parent, explicit
dependencies, and a normalized firmware binding. Nodes are one of:

- `Virtual`: implemented entirely by Axvisor;
- `HostPassthrough`: retains a checked host mapping and fixed reservations;
- `HostReplacement`: preserves host firmware identity and resources but uses a
  virtual implementation, such as the AArch64 VGIC;
- `FirmwareOnly`: a bus, provider, or container with no runtime device.

The graph rejects duplicate IDs, missing or repeated dependencies, and cycles.
Sealing produces a deterministic topological order. A passthrough node stores
plain validated descriptors rather than parser references or borrowed host
objects.

## Two-phase factories

The graph stores the exact `Arc<dyn DeviceFactory>` used for both phases:

1. `declare()` validates immutable compatibility configuration and returns
   named resource requirements without touching runtime state.
2. `build()` receives an exclusive `DeviceBuildContext` and can obtain a
   resource only through its named slot.

Runtime construction calls the factory retained by the resolved node. It does
not look up another implementation by device type. This prevents a registry
change between declaration and construction from invalidating the plan.

Machine profiles may still contain internal fixed device descriptors for
stable platform ABI. They are translated into `Fixed` requests before
planning; a factory must use the resolved address and IRQ returned by its
build context. User-visible `GuestConfig` does not gain raw allocation
overrides.

## Resource namespaces and allocation

Resources are typed and named:

- MMIO and PIO ranges;
- wired inputs keyed by `(InterruptControllerId, ControllerInputId)`;
- host IRQ identities;
- MSI DeviceID/EventID keyed by ITS;
- controller-global LPIs.

Automatic pools, fixed allowlists, and reservations are separate. Guest RAM,
architecture windows, host passthrough mappings, and controller-internal
regions are reserved before an automatically assigned aperture is considered.

Planning is a single transaction:

1. validate nodes, slots, sizes, alignments, ranges, and arithmetic;
2. import architecture and host reservations;
3. place every fixed request first;
4. sort automatic requests by node ID, resource kind, and slot;
5. allocate the lowest matching value;
6. publish claims only after the complete graph succeeds.

This is implemented locally rather than with `vm-allocator`. Its interval
allocator does not provide owner-rich cross-domain conflicts, shared IRQ
compatibility, MSI/LPI compound namespaces, one-shot claims, or VM-wide
rollback. The lowest-range search remains private so it can be replaced later
without changing the public domain model.

## Claims, leases, and runtime transactions

A planned slot transitions only through `planned -> issued -> leased`.
Duplicate issue or consumption fails. Dropping an unfinished claim or a lease
returns the slot to the planned state, so a failed build can retry the same
lowest value. Non-runtime nodes consume their fixed claims into graph-owned
VM-lifetime leases.

An IRQ slot is resolved by controller ID. `DeviceBuildContext::irq()` obtains
the controller-owned `WiredIrqInput`, creates an independent source line, and
retains its endpoint registration with the resource lease. Edge lines expose
`pulse`; level lines expose `assert`/`deassert`; shared level sources are
wired-OR, and dropping a source withdraws its assertion.

`DeviceBundle` atomically commits devices, controller capabilities, endpoints,
services, grants, lifecycle hooks, and leases. Any validation or registration
failure restores every runtime index. Controllers must be registered before
dependent nodes. After every graph node is built and every claim is leased,
the runtime topology is sealed and further registration is rejected.

## Firmware and guest address space

Firmware serialization occurs after resolution and before runtime build. A
firmware builder may combine resolved slots with immutable architecture facts,
such as APIC IDs or an Arm GIC topology, but it must not read mutable runtime
devices or re-run allocation.

For identity-mapped passthrough VMs, host mappings form the baseline. Guest
RAM, boot data, virtual-device MMIO, and host-replacement capture windows are
removed from that baseline. `HostPassthrough` mappings stay mapped. An
unrepresentable overlap is an initialization error rather than an implicit
overwrite.

The graph retains normalized FDT/ACPI identity. Architecture firmware builders
currently consume their structured machine or host snapshots together with
resolved numeric slots; they do not copy arbitrary host AML.

## Architecture-specific construction

### AArch64

AArch64 snapshots host GIC, serial, timer, and FDT identity into an immutable
firmware plan. It supports all configured GIC redistributor regions and stride,
and uses the same `ArmVgicConfig` for runtime construction and FDT. The VGIC is
a `HostReplacement`: host GIC apertures and physical SPI identities are fixed,
while all guest distributor/redistributor state remains virtual.

The existing mainline `VgicCore`, timer, LR save/restore, and physical SPI
quiesce/drain/deactivate lifecycle remain authoritative. A host without a
usable ITS cannot advertise guest ITS or satisfy MSI requirements. Physical
MSI remains explicitly unsupported without a platform `PhysicalMsiRouter`.

### RISC-V

RISC-V retains PLIC hart/context ownership and initialization order. The PLIC
factory consumes its resolved aperture, and final FDT generation occurs from
the architecture profile after graph resolution.

### x86

x86 retains LAPIC, IOAPIC, PIT, APIC-access, and vCPU ordering. Small immutable
CPU, interrupt, PCI, and firmware plans are composed from the resolved graph.
All graph-resolved PIO ranges are installed in the VM-exit trap set.

The direct Linux path builds RSDP, XSDT, FADT, FACS, DSDT, MADT, and SPCR with
`acpi_tables 0.2.1`. The image is checked and placed in `0xe0000..0x100000`;
its RSDP is 16-byte aligned, the range is reserved in E820, and
`boot_params.acpi_rsdp_addr` points to it. The MP table is generated from the
same APIC plan as an explicit `acpi=off` fallback.

The firmware path exposes the same logical CPU, IOAPIC, serial, PCI routing,
and ACPI content through QEMU-compatible PIO fw_cfg. Fixed claims cover
selector/data `0x510..0x512` and DMA `0x514..0x51c`; payload files are
`etc/acpi/tables`, `etc/acpi/rsdp`, and `etc/table-loader`. The current PCI
configuration mechanism is legacy mechanism 1, so no MCFG is emitted. No HPET
or unimplemented PM/GPE register is invented.

### LoongArch

LoongArch retains IOCSR and EXTIOI/PCH-PIC/PCH-MSI cascading. Its MMIO fw_cfg
transport consumes the resolved graph address while the shared ACPI arena,
RSDP/XSDT/FADT/FACS composition, and loader planning are reused. LoongArch MADT
entries and platform AML remain architecture-owned.

## Failure reporting and validation

Domain errors derive `thiserror::Error` and identify the failed phase. Resource
conflicts include namespace, value, existing owner, and requester. Unsupported
firmware or hardware combinations return explicit errors rather than choosing
an undeclared fallback.

Unit tests are limited to invariants whose failure would invalidate the model:
deterministic fixed/automatic placement, namespace and sharing rules, claim and
bundle rollback, ACPI pointer/checksum closure, and fw_cfg selector/DMA
behavior. Cross-boundary tests verify resolved resources through firmware and
runtime, x86 direct/OVMF ACPI, the MP fallback, and architecture controller
boot paths.
