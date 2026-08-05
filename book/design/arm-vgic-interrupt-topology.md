# AArch64 VGIC and virtual-device resource topology

Status: implementation baseline for PR #1718 rewrite

## Scope and references

This design covers deterministic virtual-device resources and the non-secure
Group 1 interrupt paths used by ordinary Linux and ArceOS guests: GICv2,
GICv3, ITS/LPI, software wired/MSI delivery, and preconfigured physical SPI
backing. Secure Group 0/Group 1S, GICv3.1 ESPI, GICv4/vPE, nested
virtualization, and an external live-migration format are out of scope.

The semantic references are Arm IHI 0048 (GICv2), Arm IHI 0069 (GICv3),
Linux v7.1 KVM VGIC documentation, and QEMU 10.1.0. Pull request #1612 is
prior art rather than code to merge. This rewrite keeps its useful
model-to-plan-to-claim-to-bundle direction while retaining the current
`DeviceRuntime`, typed services and grants, lifecycle, timer, LR, and physical
SPI implementations from `dev`.

## Dependency direction and module size

Architecture initialization policy remains architecture-owned:

```text
axdevice_base
  typed interrupt IDs, IrqLine, controller capability traits
        ^
axdevice
  model requirements, resource plan, claims, bundle registrations
        ^
axvm arch::{aarch64,riscv64,x86_64,loongarch64}::vm
  independent construction and registration order
        ^
architecture controller implementations
```

Shared code supplies allocation, validation, rollback, and registration
mechanisms. It does not prescribe one cross-architecture device order.
AArch64 registers the VGIC before ordinary IRQ devices; RISC-V retains PLIC
hart/context setup; x86 retains LAPIC/IOAPIC/PIT and APIC-access ordering;
LoongArch retains IOCSR and EXTIOI/PCH-PIC cascading.

Resource code is split by owned invariant. Address, wired IRQ, MSI, claim
lifecycle, and immutable resolved data are separate modules. `ResourcePools`
contains three small sub-pools instead of one structure containing every map.
The AArch64 plan follows the same rule: firmware identity, VGIC layout, and
device resources are composed values, not fields accumulated in one large
mutable VM structure.

## Allocator choice

`vm-allocator` provides useful lowest-first address and numeric ID allocation,
but it does not model resource namespaces, owners, shared IRQ compatibility,
compound DeviceID/EventID/LPI reservations, or one-shot transactional claims.
The repository also has similar allocation code under `rdif-pcie`; making the
architecture-neutral device framework depend on a PCI driver-interface crate
would reverse the intended dependency direction.

The planner therefore uses a small private range-search mechanism and keeps
the domain semantics in typed modules. Allocation state is created locally
for one call to `plan()` and is published only after every request succeeds.
This provides transaction rollback without snapshots or a second persistent
allocator state. If a neutral workspace allocator is introduced later, only
the private range-search module should change.

## State ownership

There is no independent interrupt fabric. Each concrete virtual interrupt
controller is the only owner of enable, pending, line level, active, priority,
route/target, and EOI state. `WiredIrqInput` owns only electrical source
aggregation:

- edge sources call `pulse()`;
- level sources call `assert()` and `deassert()`;
- shared level sources combine as wired-OR;
- dropping an asserted source withdraws that source.

AArch64 has one `Arc<VgicCore>` per VM. The same allocation remains typed for
vCPU and physical IRQ operations and is also registered as
`Arc<dyn VirtualInterruptController>` for virtual devices. GICD, GICC/GICR,
ICC, and ITS frontends decode architectural accesses into that core; none is
a second state owner and none forwards guest writes to host GICD/GICR state.

Every INTID distinguishes a pending latch from the current line level. LRs are
a finite hardware cache of canonical state. Overflow stays in software and is
refilled by priority and routing. The mainline timer, LR save/restore, and
physical SPI quiesce/drain/deactivate implementations remain authoritative.

## Resource planning and claims

Namespaces are explicit. MMIO, PIO, `(controller, input)`,
`(controller, ITS, DeviceID, EventID)`, controller-global LPI, and host IRQ
identities never compare as untyped integers. Models declare named slots;
architecture code supplies automatic pools, fixed allowlists, and internal
reservations.

Planning is deterministic:

1. validate model requirements and architecture pools;
2. copy architecture/controller reservations into local allocation state;
3. reserve fixed requests in stable device-ID and slot order;
4. allocate automatic requests in the same stable order;
5. choose the lowest aligned value from the matching namespace;
6. publish immutable resources and one-shot claims only after all requests
   succeed.

Automatic pools and fixed allowlists are separate. A host-derived fixed GIC,
UART, timer, or ITS address cannot silently expand the range used for new
automatic devices.

Conflict detection is part of reservation, not an `is_free()` query followed
by a later claim. Errors carry the namespace, value/range, existing owner, and
requester. Owner lookup is diagnostic only.

Claims transition `planned -> issued -> leased`. Issuing one device is atomic.
Dropping an unconsumed claim or a lease rolls that slot back to `planned`, so a
failed device build can retry the same deterministic lowest resource. A
device build cannot finish with unconsumed slots, and VM sealing cannot finish
while any planned slot lacks a retained lease. A build context validates the
slot kind and prepares the controller endpoint before consuming the claim; a
factory that handles an accessor error therefore cannot accidentally discard
the claim and later commit an incomplete bundle.

## Runtime controller and endpoint registration

`VirtualInterruptController` remains a narrow capability: it converts a
planned controller input and trigger into controller-owned `WiredIrqInput`.
It does not expose MMIO, system registers, vCPU state, host IRQs, firmware,
EOI, or architecture routing. MSI is a separate optional capability.

The runtime builder accepts `ControllerRegistration` as a `DeviceBundle`
member. Registration validates the declared ID against `controller.id()`,
rejects duplicate IDs, and installs wired and optional message capabilities
atomically with controller frontend devices. Ordinary factories receive an
exclusive `DeviceBuildContext`; `mmio(slot)`, `pio(slot)`, `irq(slot)`, and
`msi(slot)` consume claims rather than raw configuration fields.

Endpoint registrations retain both the controller-created endpoint and its
resource lease. Runtime validation rejects missing controllers, mismatched
IDs, endpoint values that differ from the plan, and incompatible sharing. A
failed bundle registration restores device, bus, controller, endpoint,
service, grant, lifecycle, and pollable indices to their prior state.

The current mainline services, grants, access ports, lifecycle transitions,
and topology seal remain unchanged. The builder only permits architecture
code to register controller bundles before sealing.

## AArch64 immutable plan and firmware

AArch64 produces one immutable `Aarch64VmPlan` before final guest FDT
serialization. It composes resolved device resources, firmware identity, and
a complete `ArmVgicConfig`. Device construction consumes that plan and must
not probe again, allocate again, or reconstruct configuration by downcasting
a registered GIC frontend.

Host and guest GIC versions must match. GICv2 supports at most eight vCPUs.
GICv3 validates unique MPIDR affinities against all configured redistributor
regions and their stride. A host without a usable ITS produces neither guest
ITS firmware nor an MSI capability; an MSI requirement then fails explicitly.

Guest GIC firmware is sanitized from the same configuration:

- GICv2 preserves selected host GICD/GICC layout while removing GICH/GICV,
  maintenance, secure, and host-only properties;
- GICv3 preserves GICD, every selected GICR region, redistributor stride, and
  interrupt-cell layout;
- ITS nodes exist only for matching host ITS capabilities;
- phandles are preserved when safe or rewritten consistently;
- VGIC MMIO ranges remain stage-2 traps, never host-register mappings.

All GICD, GICC/GICR, and ITS apertures are checked for address-end overflow
and pairwise overlap before the profile mutates machine configuration. GICR
capacity is computed across all regions, and VM-local ITS IDs must be unique.
An ITS node must describe exactly one register aperture.

Physical SPIs are selected while planning and require
`guest INTID == host INTID`. Host IRQ identity, physical trigger, and host
route are immutable to the guest. Guest writes update virtual state only.
The existing AArch64 route slot gains VM-lifetime reservation state; it is not
replaced by a second generic host-IRQ registry.

## Locking and callbacks

The lock order is:

```text
resource claim state
  -> device/controller registry
    -> VGIC distributor/per-vCPU/ITS state
      -> backend-local LR or physical-route state
```

No resource or device-registry lock is acquired while holding VGIC state.
VGIC critical sections update canonical state and produce actions. vCPU
wakeups, IPIs, maintenance notification, host acknowledge/deactivate, and
physical route operations run after releasing the VGIC lock.

A vCPU binding follows:

```text
fold saved LR state -> refill -> restore -> guest run -> save -> fold
```

Pause/resume preserves HCR, VMCR, APR, and all LRs. IRQ-facing VGIC locks are
IRQ-safe because injection can re-enter on the same physical CPU. Mainline
physical SPI reader draining and deactivate ordering remain unchanged.

## Failure and rollback

Planning is all-or-nothing. A validation or exhaustion failure publishes no
plan. A failed factory build drops endpoints and leases, returning claims to
`planned`. A failed bundle registration leaves all runtime indices unchanged.
A failed AArch64 physical binding tears down host routing before dropping the
VGIC registration. Destruction releases the VM-lifetime route reservation.

Firmware serialization reads only immutable resolved resources. It therefore
cannot describe an address, IRQ, GICR region, or ITS instance different from
the one later consumed by the runtime.

## Validation matrix

| Area | Required evidence |
| --- | --- |
| Planner | fixed-before-auto, stable lowest-first, alignment/overflow/exhaustion, duplicate device/slot, retry after build failure |
| Namespaces | same input on different controllers, exclusive conflict, shared trigger mismatch, ITS isolation, controller-global LPI |
| Claims | duplicate issue/consume, unconsumed rejection, lease rollback, bundle rollback, seal verification |
| Controller contract | dyn-only edge/level/shared lines, duplicate/missing/ID-mismatched controller, source-drop deassert |
| VGIC | SGI/PPI/SPI, pending/active, line/latch, priority/route, LR overflow, maintenance, EOI/DIR |
| Firmware | VGIC config, runtime resources, GIC/ITS nodes, and device interrupt properties originate from one plan |
| Physical backing | fixed identity/trigger/route, duplicate host SPI rejection, quiesce/drain, guest EOI/DIR to host deactivate |
| System | QEMU GICv2 two-vCPU timer stress, GICv3+ITS four-vCPU timer stress with Linux ITS initialization assertion, and four-architecture smoke |
