# Portable AHCI Driver and Multi-device Runtime Design

## Status

This document defines the review baseline for extracting the AHCI implementation
from `ax-driver` and exposing every attached SATA disk behind one HBA. The
implementation is high risk because it introduces a portable MMIO/DMA driver
crate, shared hard-IRQ fan-out, NCQ, and a controller-to-many-devices runtime
boundary.

The design must be reviewed before the implementation is considered
merge-ready. Phytium SATA support remains build- and simulation-validated until
physical-board evidence is recorded.

## Problem and success criteria

The current AHCI implementation is coupled to ArceOS probe, MMIO, DMA, and
registration APIs. It selects one port, uses command slot zero, and reports one
block device even when the HBA implements multiple SATA ports. Copying the
controller object is invalid because every copy would reset the same HBA,
modify the same global interrupt enable, and tear down sibling ports.

The users of this change are ArceOS, StarryOS, Axvisor, and future Rust kernels
that need the same AHCI hardware state machine with different probe and runtime
services.

The implementation is complete when:

- `ahci-driver` is `no_std` and has no OS, PCI, FDT, or `rdrive` dependency;
- one HBA reset produces one independently registered block device per attached
  SATA port, ordered by physical port number;
- two ports can execute NCQ requests and complete out of order through one
  physical IRQ registration;
- a port failure does not stop a healthy sibling, while a host failure stops the
  complete group;
- existing PCI AHCI and LS2K feature names remain usable;
- QEMU exposes two disks on one ICH9 AHCI controller and concurrent read/write
  verification succeeds.

Hotplug, port multipliers, ATAPI, enclosure-management bridges, polling I/O,
and PHY-dependent DWC/Rockchip controllers are not part of this change.

## Prior art and alternatives

The register and command semantics follow Intel AHCI 1.3.1. NCQ tags are command
slots, the effective queue depth is bounded by `CAP.NCS`, IDENTIFY DEVICE, and
32, and queued commands publish `PxSACT` before `PxCI`. Linux libahci is used as
prior art for per-port state, shared host interrupt demultiplexing, and fatal
error recovery; its Linux locking and object model are not copied.

The considered alternatives are:

| Alternative | Result |
| --- | --- |
| Keep the current implementation | Leaves OS coupling, single-port exposure, and slot-zero serialization. |
| Register several copies of the current controller | Rejected: reset, GHC, MMIO, IRQ, and shutdown ownership would conflict. |
| Represent ports as hardware queues of one block device | Rejected: ports have independent media, geometry, failure, and filesystem identity. |
| Add AHCI-specific registration hooks to ArceOS | Rejected: it would duplicate a generally useful controller-to-many-devices boundary in OS glue. |
| Add a portable host group plus independent port controllers | Selected: it matches hardware ownership and preserves the existing per-device block runtime. |

## Layering and ownership

### Driver core

`ahci-driver` owns:

- the mapped `mmio_api::Mmio` for the lifetime of the host and every child;
- typed HBA and port register access built with `tock-registers`;
- HBA and port state machines;
- coherent command lists, received FIS areas, command tables, and PRDTs;
- accepted request and `InFlightDma` ownership until terminal completion;
- NCQ tag allocation and completion state;
- interrupt extraction and preallocated per-port event publication.

The crate receives a `DeviceDma` capability. It narrows the DMA address mask to
the HBA capability and reports the resulting constraints through `rdif-block`.
It never maps MMIO, creates a DMA domain, registers an IRQ, starts a task, or
parses firmware.

### Capability and runtime boundary

`rdif-block` gains a controller group boundary in addition to the existing
single-device controller:

- `BlockControllerGroup` advances one hardware-owner lifecycle;
- `BlockGroupMember` transfers a stable member id and one
  `BlockController`;
- `SharedIrqEndpoint` transfers exactly one shared hard-IRQ handler;
- `SharedHardIrqHandler` acknowledges hardware into a caller-provided
  `GroupIrqSink`;
- `GroupIrqSink` publishes member-local queue and control events without
  allocating in hard IRQ context.

The member id is the AHCI physical port number. Queue ids remain local to the
member block device.

The block runtime owns one group controller task, one physical IRQ token, and
one ordinary `BlockDeviceHandle` per successful member. All member queue and
controller targets are prepared before the physical IRQ is enabled. The hard
IRQ action contains an immutable, preallocated target table.

### OS glue

`ax-driver` owns PCI/FDT matching, firmware resource preparation, MMIO mapping,
`DeviceDma` construction, IRQ binding, and platform-profile selection. One
platform descriptor stores one block group. The runtime expands the group into
member disks only after host initialization.

PCI glue continues to require SATA/AHCI class code, an MMIO BAR, bus mastering,
and an IRQ. FDT glue accepts Loongson AHCI and `generic-ahci`. It does not claim
`snps,dwc-ahci` or another compatible requiring a PHY boundary that the
workspace does not provide.

## Lifecycle

### Startup

1. OS glue maps the complete HBA aperture and constructs `AhciHost`.
2. The group controller masks host interrupts, performs exactly one HBA reset,
   enables AHCI mode, applies the selected platform profile, and reads `PI`.
3. Implemented ports are prepared in ascending physical order. Ports with an
   unsupported device signature are excluded.
4. A member controller and its queue state are created for each candidate SATA
   port. Empty or failed ports may fail independently during link/IDENTIFY
   startup without aborting healthy siblings.
5. The runtime starts all members with IRQ generation masked. This creates
   queue/controller latches and starts IDENTIFY commands for link-ready ports.
6. The runtime creates one shared IRQ action with every member target, registers
   it disabled, publishes all state, enables per-port interrupts, enables the
   host interrupt, and finally enables the OS IRQ token.
7. IDENTIFY completion publishes geometry and negotiated capabilities. Only
   members that reach `Ready` are exposed to the filesystem.

No data-completion polling fallback is permitted. Register-only reset, engine,
and link transitions use runtime-owned retry timers.

### Normal I/O

Each port owns one `HardwareQueue`. Its effective depth is:

```text
min(HBA CAP.NCS + 1, IDENTIFY queue depth, 32)
```

when both HBA and device support NCQ, otherwise one.

Submission reserves a free tag and all DMA ownership before removing a request
from the runtime batch. Command descriptors are prepared for every accepted
request. A Release fence publishes descriptor writes, then queued commands add
their bits to `PxSACT` before adding the same bits to `PxCI`.

One PRDT entry describes at most 4 MiB. A contiguous prepared DMA segment is
split into as many PRDT entries as required, subject to the command-table and
ATA sector-count limits.

The host IRQ handler reads the global interrupt status once. For every asserted
port it reads and masks `PxIS`, clears only that port's status, records the
status in preallocated port state, and publishes the member id to
`GroupIrqSink`. It performs no allocation, DMA completion, queue locking,
filesystem call, or task scheduling.

The member maintenance task compares software-issued tags with cleared
`PxSACT/PxCI` bits. It may complete tags in any order and returns each DMA
buffer exactly once.

Flush is nonqueued and is submitted only after data requests drain. FUA is
encoded only for supported write commands.

### Failure and shutdown

A task-file, link, or interface-fatal error masks the affected port and starts
port-local recovery. DMA remains owned or quarantined until the command engine
is confirmed stopped. Healthy ports remain operational.

A host reset failure or loss of the complete MMIO/IRQ capability fails every
member.

Group shutdown follows this order:

1. stop accepting requests on every member;
2. mask host and port interrupt generation;
3. disable and synchronize the one OS IRQ token;
4. quiesce every member queue;
5. stop `PxCMD.ST`, wait for `PxCMD.CR` to clear, stop `PxCMD.FRE`, and wait for
   `PxCMD.FR` to clear;
6. complete or quarantine all remaining DMA;
7. shut down the host and drop the MMIO mapping after the last member.

Dropping or failing one member never clears the host interrupt enable or unmaps
the HBA.

## Compatibility and rollback

The `ahci` feature remains the PCI entry point. `ahci-fdt` is the generic FDT
entry point, and `ls2k1000-ahci` remains a compatibility alias using the LS2K
profile. `NcqPolicy::Disabled` provides a platform/debug rollback to the
nonqueued depth-one command path without restoring the old driver.

Root selection keeps the existing `/dev/sdX`, PARTUUID, and PARTLABEL
semantics. AHCI members are emitted in physical port order; explicit persistent
root identifiers remain recommended when several storage controllers exist.

The unused legacy AHCI dependency is deleted without a compatibility shim.

## Validation matrix

| Claim or risk | Evidence |
| --- | --- |
| One HBA owns several disks | Fake-MMIO host test discovers two ports after one reset. |
| Shared IRQ is lossless | Runtime test publishes two member events from one IRQ and observes one registration and two wakeups. |
| NCQ ownership is safe | Queue tests cover full depth, partial batch acceptance, out-of-order completion, and one-time DMA return. |
| PRDT limits are correct | Boundary tests split at 4 MiB and reject odd, oversized, or over-capacity transfers. |
| Port errors are isolated | Two-port test fails one port while the sibling continues and completes I/O. |
| Teardown is DMA-safe | Tests assert IRQ synchronization precedes queue/host release and active DMA is quarantined when the engine cannot stop. |
| Platform compatibility | Targeted LS2K/JL/Phytium builds and PCI/FDT profile tests. |
| End-to-end multi-disk behavior | QEMU Q35/ICH9 AHCI with two disks, NCQ depth greater than one on both, and concurrent 20 MiB read/write/hash verification. |
