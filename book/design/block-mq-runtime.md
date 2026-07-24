# IRQ-Driven Block Multi-Queue Runtime

## Status

This document defines the first incompatible migration of the TGOSKits block
stack to an interrupt-driven, SMP-aware multi-queue runtime. It is the design
contract for the implementation; changes to the ownership or execution model
must update this document before they are merged.

The first supported devices are:

- PCI NVMe, with MSI-X multi-queue and an explicit single-queue INTx mode.
- RK3588 DWCMSHC eMMC on OrangePi 5 Plus, using ADMA2.

All other block backends are unavailable through `ax-driver` until they are
migrated. Their reusable hardware crates may remain in the tree.

## Problem

The existing block runtime mixes borrowed and owned requests, direct task IDs,
completion polling, IRQ-triggered polling, and a global drain task. Drivers may
therefore share queue state with hard IRQ handlers, and a missed or unavailable
IRQ silently changes the execution model to polling. This prevents a queue from
having one auditable owner and makes SMP scaling, DMA lifetime, teardown, and
hardware boundary enforcement difficult to prove.

The replacement must provide these observable properties:

1. Runtime data I/O never polls hardware for completion.
2. A hard IRQ acknowledges the device and only activates deferred work.
3. One maintenance task exclusively owns each hardware queue.
4. Callers submit owned requests through bounded channels and receive terminal
   results through one-shot completion subscriptions.
5. Every DMA and queue limit is validated before hardware ownership begins.
6. IRQ registrations own boxed handlers and outlive every possible callback.
7. NVMe and RK3588 eMMC boot and perform verified I/O on their target systems.

## Reference Model

The software/hardware queue split and native batching contract follow Linux
blk-mq as implemented in both Linux v7.2-rc4, tag object
`6946cd5d0aa4dd10a414ddcb7a10844fdb0ad345`, commit
`1590cf0329716306e948a8fc29f1d3ee87d3989f` (2026-07-19), and Torvalds'
current master commit
`48a5a7ab8d6ab7090564339e039c421f315de912` (verified 2026-07-24):

- [blk-mq documentation](https://docs.kernel.org/block/blk-mq.html)
- [`block/blk-mq.c` at current master](https://kernel.googlesource.com/pub/scm/linux/kernel/git/torvalds/linux/+/48a5a7ab8d6ab7090564339e039c421f315de912/block/blk-mq.c)
- [`include/linux/blk-mq.h` at current master](https://kernel.googlesource.com/pub/scm/linux/kernel/git/torvalds/linux/+/48a5a7ab8d6ab7090564339e039c421f315de912/include/linux/blk-mq.h)

- a CPU-local software submission context selects a hardware context;
- a hardware context owns tags and the device submission/completion queue;
- `queue_rqs` lets blk-mq present a same-device list and leaves unconsumed
  requests owned by the block layer;
- a scalar `queue_rq` may defer publication while `bd.last` is false, and
  `commit_rqs` must publish accepted work when a partial dispatch or error
  makes the earlier `last = false` promise untrue;
- the block layer creates batching opportunities and owns requeue/backpressure,
  while the driver owns descriptor construction and the hardware-specific
  commit, such as an NVMe SQ tail doorbell;
- completion ordering is not implied across hardware contexts;
- queue limits are enforced before dispatch.

The interrupt split follows the Linux v7.2-rc4 PREEMPT_RT model documented in
[`Documentation/core-api/real-time/differences.rst`](https://kernel.googlesource.com/pub/scm/linux/kernel/git/next/linux-next/%2B/1590cf0329716306e948a8fc29f1d3ee87d3989f/Documentation/core-api/real-time/differences.rst)
and the request/free/synchronize lifetime in
[`kernel/irq/manage.c`](https://kernel.googlesource.com/pub/scm/linux/kernel/git/next/linux-next/%2B/1590cf0329716306e948a8fc29f1d3ee87d3989f/kernel/irq/manage.c):
hard IRQ work is restricted to source identification, device acknowledgement
or masking, publishing preallocated state, and activating deferred task
context. TGOSKits uses dedicated maintenance tasks rather than importing Linux
threaded-IRQ or softirq infrastructure.

NVMe register, queue, PRP, interrupt, Identify, MDTS, and queue-count behavior
is checked against [NVM Express Base Specification 2.2, ratified
2025-03-11](https://nvmexpress.org/wp-content/uploads/NVM-Express-Base-Specification-Revision-2.2-2025.03.11-Ratified.pdf)
and current Linux
[`drivers/nvme/host/pci.c`](https://kernel.googlesource.com/pub/scm/linux/kernel/git/torvalds/linux/+/48a5a7ab8d6ab7090564339e039c421f315de912/drivers/nvme/host/pci.c).
Linux NVMe implements both `queue_rqs`, which copies a prepared request list
and writes the SQ doorbell once, and `queue_rq + bd.last`, with `commit_rqs` as
the error/partial-dispatch publication path. TGOSKits exposes one owned batch
operation instead of carrying both Linux entry shapes, but preserves their
ownership and commit semantics.
RK3588 vendor register, clock, reset, and SDHCI behavior is checked against the
same Linux commit's
[`drivers/mmc/host/sdhci-of-dwcmshc.c`](https://kernel.googlesource.com/pub/scm/linux/kernel/git/next/linux-next/%2B/1590cf0329716306e948a8fc29f1d3ee87d3989f/drivers/mmc/host/sdhci-of-dwcmshc.c)
and
[`drivers/mmc/host/sdhci.c`](https://kernel.googlesource.com/pub/scm/linux/kernel/git/next/linux-next/%2B/1590cf0329716306e948a8fc29f1d3ee87d3989f/drivers/mmc/host/sdhci.c).
QEMU root-disk arguments follow the
[QEMU NVMe device documentation](https://www.qemu.org/docs/master/system/devices/nvme.html)
as accessed 2026-07-23: `max_ioqpairs=64`, `msix_qsize=65`; the explicit INTx
validation case advertises only one MSI-X vector and therefore rejects MSI-X
setup before selecting INTx.

## Layering Without a New Crate

No block-runtime crate is added.

- `rdif-block` owns the portable capability contract and request/limit types.
- Hardware driver crates own registers, descriptors, controller state machines,
  hardware queues, tags, DMA ownership after submission, and IRQ acknowledgement.
- `ax-driver` owns probe, MMIO/PCI/FDT discovery, DMA capability construction,
  IRQ source resolution, and publication through `rdrive`.
- `ax-fs-ng::block::runtime` owns channels, software queues, hardware-context
  maintenance tasks, completion subscriptions, barriers, timeouts, and teardown.
- `axruntime` implements the task/IRQ capability adapter and invokes the
  post-SMP-online expansion hook.

Portable drivers must not depend on `ax-task`, filesystem types, or scheduler
APIs. Hard IRQ callbacks must not call `rdrive`.

## Portable Interfaces

### Controller lifecycle

`rdif-block::BlockController` owns a device state machine. The runtime advances
it with typed inputs:

- initial bootstrap with a target of one I/O queue;
- an acknowledged control IRQ event;
- a retry for a register-only state;
- a post-SMP target queue count;
- IRQ rearm after deferred drain;
- IRQ quiescing and a queue watchdog failure;
- shutdown.

Each advance returns a state plus zero or more newly available IRQ endpoints
and hardware queues. The states are:

- `RegisterPending`: a register has not reached the requested state;
- `WaitingForIrq`: progress requires a published IRQ event;
- `Ready`: the current bootstrap or scale target is operational.

Only `RegisterPending` may be advanced in a busy loop. The runtime checks one
overall deadline around that loop. Command and data completions must transition
through IRQ events.

### IRQ endpoint

Each endpoint owns:

- a typed IRQ source identifier and queue affinity mask;
- `Box<dyn HardIrqHandler + Send + 'static>`;
- no scheduler, filesystem, queue, or registry object.

`HardIrqHandler::ack` returns `IrqAck`:

- `Spurious`: this action did not own the interrupt;
- `Cleared`: hardware acknowledgement is complete;
- `MaskedNeedsRearm`: the source is masked and deferred work must explicitly
  rearm it after draining.

The acknowledgement also carries a queue bitmap and an opaque controller event.
It never carries borrowed memory or invokes a completion callback.

### Hardware queue

`HardwareQueue` is move-only and is owned by one maintenance task. Its data path
contains only:

- `submit_batch_owned`, which receives an ordered `OwnedRequestBatch`, removes
  only the accepted prefix, and reports the corresponding request IDs in the
  same order;
- a result containing the accepted count and `Continue`, `QueueFull`, or
  `Fatal` disposition; every unaccepted request remains runtime-owned and in
  its original order;
- `commit_submissions`, which publishes descriptors accepted by the preceding
  batch and is called exactly once whenever ownership moved, including partial
  acceptance and fatal/error exits;
- `drain_completions`, which is called only after a matching IRQ event and
  transfers terminal results and completed DMA to a sink;
- `shutdown`, which quiesces hardware and returns or quarantines every remaining
  DMA allocation.

The driver must not remove a request until its tag, DMA mapping, and descriptor
state are reserved. The runtime treats a mismatch between the removed prefix,
accepted count, and accepted-ID stream as a fatal driver-contract violation,
but still terminates every request whose metadata or DMA remains runtime-owned.

There is no request polling or cancellation interface. Dropping a caller's
completion subscription does not cancel hardware I/O.

## Runtime Data Flow

```text
caller
  -> per-CPU bounded submission channel
  -> mapped hctx maintenance task groups up to max_submit_batch
  -> exclusively owned HardwareQueue
  -> one batch commit

hard IRQ
  -> boxed handler ack
  -> preallocated atomic event latch
  -> IRQ-safe maintenance-task notify

maintenance task
  -> drain acknowledged completions
  -> publish one-shot completion
  -> wake the subscribing caller
```

The submission channel capacity defaults to the mapped hardware queue depth.
A full channel blocks ordinary callers; `NOWAIT` returns backpressure without
entering hardware. A single request follows the same channel and is
automatically grouped with adjacent submissions. `submit_owned` returns one
`CompletionSubscription`; `submit_batch_owned` returns an ordered
`CompletionGroup`. Both provide blocking `recv` only. Hardware may complete a
group out of order, but the group reports results in submission order. The
filesystem's synchronous methods are wrappers over submit plus `recv`; they are
not polling APIs.

Each hctx round-robins its mapped per-CPU channels, first consumes any preserved
partial batch, and bounds a native batch by free tags, queue depth, and
`max_submit_batch`. A `QueueFull` suffix is retried without reconstructing DMA.
At most one normal commit is performed per batch operation.

The IRQ event latch uses release/acquire publication and coalesces repeated
queue events. A maintenance task drains the latch before sleeping and the
notification primitive checks a pending bit while enqueueing the waiter, so an
IRQ between the final check and blocking cannot be lost.

A request watchdog fails the device and asks the controller state machine to
reset it after the queue has quarantined or returned every DMA owner. It must
not inspect a completion queue or resubmit a polling query.

## SMP and Ordering

The bootstrap CPU creates the control task and one I/O hctx before root
filesystem discovery. After every secondary CPU has initialized its scheduler,
IPI path, and local IRQ state, `axruntime` calls
`ax_fs_ng::block::runtime::online_smp()`.

The expansion path:

1. asks the controller state machine for additional queues;
2. creates each hctx and its bounded submission channel;
3. registers its boxed IRQ handler disabled with fixed CPU affinity;
4. publishes the hctx to submitters;
5. enables the IRQ source.

The number of NVMe I/O queues is the minimum of online CPUs, controller queue
capacity, available I/O MSI-X vectors, and 64. DWCMSHC exposes one hctx.

Requests in different hctxs have no implicit ordering. A device-level flush
barrier stops later dispatch, waits for all earlier data requests on every
hctx, submits one flush, then releases later requests.

## Hardware Limits

`QueueLimits` expresses:

- DMA domain and address mask;
- DMA address/length alignment;
- maximum in-flight tags;
- maximum requests accepted by one native submission batch;
- maximum blocks per request;
- maximum segment count and segment length;
- an optional power-of-two boundary that no segment may cross;
- supported operations and request flags.

The transfer planner splits at block, byte, segment-count, segment-size, and
boundary limits. Validation rejects overflow, mask violations, misalignment,
unsupported flags, and buffers that cannot be represented. The hardware driver
repeats validation immediately before taking DMA ownership.

NVMe derives limits from CAP, CAP.MPSMIN as the MDTS scale base, the selected
controller page size, namespace LBA size, PRP-list capacity, DMA mask, and queue
depth. Its native batch limit is the available I/O tag depth. RK3588 derives
limits from SDHCI block count, ADMA2 descriptor length/address rules, the
128-MiB DWCMSHC ADMA boundary, and its 32-bit DMA mask; both queue depth and
native batch size are one.

## Interrupt Details

IRQ actions use non-reentrant execution, fixed affinity, and disabled-at-register
semantics.

- NVMe MSI-X has one admin vector and one vector per I/O hctx. A dedicated
  vector publishes its fixed queue bit; controller EOI completes delivery.
- NVMe INTx verifies that the PCI function asserts INTx, masks vector zero with
  `INTMS`, and returns `MaskedNeedsRearm`. The worker drains the CQ, updates the
  CQ head, and unmasks with `INTMC`.
- An NVMe hctx prepares all accepted CIDs, PRPs, and SQEs, executes one release
  barrier, and writes the SQ tail doorbell once. After an IRQ it drains all
  ready CQEs and writes the CQ head doorbell once.
- DWCMSHC reads normal/error status, performs the required W1C acknowledgement,
  and publishes command/data/error bits. The maintenance task advances the
  SD/MMC protocol state machine. OrangePi configures `RequireAdma2`; missing or
  invalid DMA is an error and never falls back to FIFO.

The hard IRQ path never drains a completion queue, allocates, copies DMA data,
or completes an OS request.

## Teardown and Failure

Teardown order is strict:

1. reject new channel submissions;
2. mask every device interrupt source;
3. disable and synchronize every IRQ registration;
4. free registrations, which drops boxed handlers after in-flight callbacks;
5. stop and join maintenance tasks;
6. quiesce queues and publish terminal failures for outstanding subscriptions;
7. return or quarantine DMA according to hardware ownership;
8. disable and release the controller/MMIO/PCI resources.

Partial initialization and SMP expansion use the same reverse-order unwind.
MSI-X setup may be completely unwound and replaced by an explicitly selected
single-queue INTx mode. No error path selects polling.

## Compatibility and Non-Goals

This is an intentional pre-1.0 breaking change. There is no compatibility
feature for borrowed requests or polling.

The first migration does not implement elevators, request merging, plugging,
I/O priorities, hotplug, multipath, multiple NVMe namespaces, discard, write
zeroes, or cross-hctx ordering other than the flush barrier.

QEMU root disks directly attached to ArceOS, StarryOS, or the Axvisor host use
NVMe. Guest-device ABIs that do not use this host block runtime are separate.

## Validation Matrix

Portable tests cover IRQ/top-half separation, lost-wakeup races, single-request
automatic grouping, one commit per batch, partial acceptance, queue-full and
fatal exits, commit failure, malformed driver ownership reports, unexpected
completion IDs, channel backpressure, dropped subscriptions, out-of-order
completion, flush barriers, DMA limits, timeout-without-polling, and
registration teardown. NVMe pure-state tests prove that multiple staged SQ
entries and multiple consumed CQ entries each produce one publication until
more work arrives.

The required QEMU validation matrix covers:

- x86_64 with 1, 2, 4, and 8 CPUs in MSI-X mode;
- x86_64 with one advertised MSI-X vector to exercise INTx;
- NVMe root filesystem read/write on aarch64, riscv64, and loongarch64;
- ArceOS, StarryOS, and Axvisor-host root filesystem boot paths.

The x86 Axvisor VMX/SVM smoke cases keep `vm_configs` empty and perform file
write, read-back, and removal through the Axvisor host shell. Guest block-device
ABIs and guest-kernel storage drivers remain outside this migration.

OrangePi 5 Plus validation covers eight CPUs, DWCMSHC eMMC root discovery,
concurrent block read/write, 512-byte and 4-KiB requests, maximum transfers,
ADMA boundary splitting, fsync, and data-integrity verification.

### 2026-07-24 x86_64 execution record

The latest-`dev` baseline and the IRQ-driven runtime used the same rootfs bytes
(`sha256:4c8f10f1a73b90282eabcddd4a4e4d3fd53b972a97779c8fa6dd5fa855784ac2`),
QEMU machine, 512 MiB memory, eight CPUs, and NVMe device arguments
`max_ioqpairs=64,msix_qsize=65`. The workload used five 4-MiB rounds, 4-KiB
operations, fsync, and QEMU per-drive snapshots. Results are median elapsed
time and throughput:

| Revision and effective mode | Write | Read | Result |
| --- | ---: | ---: | --- |
| `dbc942796` (`origin/dev`), legacy INTx | 3,995,898 us / 1.00 MiB/s | 220,382 us / 18.15 MiB/s | checksum `2673868800`, passed |
| `cb912d516` plus this working tree, 8 MSI-X I/O hctxs | 9,913,460 us / 0.40 MiB/s | 374,743 us / 10.67 MiB/s | checksum `2673868800`, passed |

The baseline driver could not use MSI-X on this ACPI setup and selected INTx,
so the table records identical advertised hardware configuration rather than
identical effective queue topology. A separate `msix_qsize=1`, one-CPU run of
the new runtime verified its explicit INTx path at 0.36 MiB/s write and
9.69 MiB/s read with the same checksum. The throughput regression is recorded
but is not a merge gate: the known scheduler wakeup latency is tracked
separately and must not be hidden by polling or driver-owned scheduling.

Explicit, reusable x86_64 configurations exercise the full MSI-X SMP matrix:

| Online CPUs / hctxs | Write median | Read median | Result |
| ---: | ---: | ---: | --- |
| 1 / 1 | 13,534,424 us / 0.29 MiB/s | 511,989 us / 7.81 MiB/s | checksum `2673868800`, passed |
| 2 / 2 | 11,983,815 us / 0.33 MiB/s | 444,114 us / 9.00 MiB/s | checksum `2673868800`, passed |
| 4 / 4 | 9,891,941 us / 0.40 MiB/s | 395,377 us / 10.11 MiB/s | checksum `2673868800`, passed |
| 8 / 8 | 9,913,460 us / 0.40 MiB/s | 374,743 us / 10.67 MiB/s | checksum `2673868800`, passed |

The non-x86 StarryOS NVMe runs use the same five-round workload and checksum:

| Architecture | Online CPUs / hctxs | IRQ mode | Write | Read |
| --- | ---: | --- | ---: | ---: |
| aarch64 | 4 / 4 | MSI-X | 0.22 MiB/s | 6.52 MiB/s |
| riscv64 | 4 / 1 | INTx, selected at initialization | 0.21 MiB/s | 6.40 MiB/s |
| loongarch64 | 4 / 1 | PCH-PIC INTx, selected at initialization | 0.28 MiB/s | 7.39 MiB/s |

The StarryOS x86_64 grouped mountinfo system case passed 42 checks and observed
`/dev/nvme0n1` as the root mount source. The ArceOS x86_64 `io_test` completed
create, write, read-back, append, seek, metadata, directory, and cleanup
operations on the axbuild-injected NVMe root filesystem. Axvisor host smoke
tests passed on x86_64 VMX, aarch64, and riscv64 after writing, reading back,
and removing a file from the host-mounted NVMe root filesystem. The LoongArch
Axvisor host also initialized NVMe, mounted ext4, and completed the initial
filesystem sync, but the full shell smoke requires QEMU-LVZ; stock QEMU does
not advertise the LVZ bit required by `loongarch_vcpu::has_hardware_support`.

The OrangePi 5 Plus physical run brought up eight CPUs and one depth-one
DWCMSHC ADMA2 hctx on CPU 0. The 512-byte, 4-KiB, 1,048,448-byte ADMA maximum,
1,048,960-byte boundary-split, and eight-task concurrent cases all passed
fsync and data verification and emitted `ORANGEPI_BLOCK_RW_BENCH_PASSED`.
