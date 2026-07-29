# IRQ-Driven Block Multi-Queue Runtime

## Status

This document defines the first incompatible migration of the TGOSKits block
stack to an interrupt-driven, SMP-aware multi-queue runtime. It is the design
contract for the implementation; changes to the ownership or execution model
must update this document before they are merged.

The hardware-validated and publicly registered devices are:

- PCI NVMe, with MSI-X multi-queue and an explicit single-queue INTx mode.
- RK3568 and RK3588 DWCMSHC eMMC, using ADMA2.
- CV181x/SG2002 SDHCI, using ADMA2.

The next SD/eMMC migration keeps traditional controllers at depth one and adds
owned-DMA, IRQ-only implementations for generic SDHCI, DW MMC IDMAC, Phytium
MCI IDMAC, CV181x/SG2002 SDHCI, StarFive JH7110 DWMMC, and the separate RK3568
DWCMSHC and DWMMC paths. An `ax-driver` feature is released only after its named
physical-board write matrix passes. JH7110 is public after its physical write
matrix passed; Phytium MCI and Rockchip DWMMC remain private after their
read-only/no-media validation outcomes. K230 source is also kept private: the
referenced Linux revision has no upstream K230 MMC implementation, and the
repository does not yet have sufficient PHY/clock/reset evidence to expose it.
Reusable hardware crates may remain in the tree without an OS registration
entry.

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
blk-mq as implemented in Torvalds' v7.2-rc5-era master commit
`62cc90241548d5570ee68e01aaba6506964e9811` (verified 2026-07-28) and the
PREEMPT_RT `linux-7.2.y-rt` commit
`0de718ad6f7842c7c2f72a785b7c0422c57231b7`, tagged `v7.2-rc4-rt3`:

- [blk-mq documentation](https://docs.kernel.org/block/blk-mq.html)
- [`block/blk-mq.c` at the verified master](https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/tree/block/blk-mq.c?id=62cc90241548d5570ee68e01aaba6506964e9811)
- [`include/linux/blk-mq.h` at the verified master](https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/tree/include/linux/blk-mq.h?id=62cc90241548d5570ee68e01aaba6506964e9811)

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

The interrupt split follows the Linux `v7.2-rc4-rt3` PREEMPT_RT model
documented in
[`Documentation/core-api/real-time/differences.rst`](https://kernel.googlesource.com/pub/scm/linux/kernel/git/next/linux-next/%2B/1590cf0329716306e948a8fc29f1d3ee87d3989f/Documentation/core-api/real-time/differences.rst)
and the request/free/synchronize lifetime in
[`kernel/irq/manage.c`](https://kernel.googlesource.com/pub/scm/linux/kernel/git/next/linux-next/%2B/1590cf0329716306e948a8fc29f1d3ee87d3989f/kernel/irq/manage.c):
hard IRQ work is restricted to source identification, device acknowledgement
or masking, publishing preallocated state, and activating deferred task
context. TGOSKits uses dedicated maintenance tasks rather than importing Linux
threaded-IRQ or softirq infrastructure.

The RT tree has no blk-mq or NVMe source delta from the corresponding mainline
files. Its relevant execution-context rule is already integrated in blk-mq:
when `force_irqthreads()` is active, completion avoids a remote completion IPI
whose softirq would only wake `ksoftirqd`. Linux also groups completion-side tag
and queue-reference release in batches of 32 (`TAG_COMP_BATCH`). TGOSKits keeps
the hctx task as the completion owner, shares one notification across an
explicit completion group, and relies on the scheduler's coalesced remote
reschedule path rather than adding a driver-owned completion thread or poller.

NVMe register, queue, PRP, interrupt, Identify, MDTS, and queue-count behavior
is checked against [NVM Express Base Specification 2.2, ratified
2025-03-11](https://nvmexpress.org/wp-content/uploads/NVM-Express-Base-Specification-Revision-2.2-2025.03.11-Ratified.pdf)
and current Linux
[`drivers/nvme/host/pci.c`](https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/tree/drivers/nvme/host/pci.c?id=62cc90241548d5570ee68e01aaba6506964e9811).
Linux NVMe implements both `queue_rqs`, which copies a prepared request list
and writes the SQ doorbell once, and `queue_rq + bd.last`, with `commit_rqs` as
the error/partial-dispatch publication path. TGOSKits exposes one owned batch
operation instead of carrying both Linux entry shapes, but preserves their
ownership and commit semantics.
RK3588 vendor register, clock, reset, and SDHCI behavior is checked against the
same Linux commit's
[`drivers/mmc/host/sdhci-of-dwcmshc.c`](https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/tree/drivers/mmc/host/sdhci-of-dwcmshc.c?id=62cc90241548d5570ee68e01aaba6506964e9811)
and
[`drivers/mmc/host/sdhci.c`](https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/tree/drivers/mmc/host/sdhci.c?id=62cc90241548d5570ee68e01aaba6506964e9811).
The wider SD/eMMC migration additionally follows:

- [`drivers/mmc/host/sdhci.h`](https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/tree/drivers/mmc/host/sdhci.h?id=62cc90241548d5570ee68e01aaba6506964e9811)
  for ADMA2 descriptor/address capability and 128 MiB SDMA/ADMA boundary
  representation;
- [`drivers/mmc/host/dw_mmc.c`](https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/tree/drivers/mmc/host/dw_mmc.c?id=62cc90241548d5570ee68e01aaba6506964e9811)
  and
  [`drivers/mmc/host/dw_mmc.h`](https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/tree/drivers/mmc/host/dw_mmc.h?id=62cc90241548d5570ee68e01aaba6506964e9811)
  for IDMAC ownership, descriptor chaining, interrupt acknowledgement, and
  reset/recovery sequencing;
- [`drivers/mmc/host/dw_mmc-rockchip.c`](https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/tree/drivers/mmc/host/dw_mmc-rockchip.c?id=62cc90241548d5570ee68e01aaba6506964e9811)
  for the separation of Rockchip clock/phase policy from the portable DW MMC
  data path.

The Phytium-specific implementation was additionally compared directly with
`/home/zhourui/linux-phytium` commit
`d50081ae7d93bf124be6722f14fb5c96600e7621` (`kernel-6.6_v3.4`),
`drivers/mmc/host/phytium-mci.c`. The portable driver preserves its separate
controller/IDMAC interrupt status, W1C acknowledgement, DTO and IDMAC terminal
conditions, explicit stop-command transition, and reset-time restoration of
power, clocks, interrupt masks, IDMAC state, and timeout registers. The vendor
tree's FIFO/PIO fallback and register-completion polling are intentionally not
carried into this IRQ-only runtime. Its descriptor address-high fields are not
evidence that the tested board DMA domain is wider than 32 bits, so the public
limit remains a verified 32-bit DMA mask.

Linux may retain PIO fallbacks for controller coverage. TGOSKits intentionally
does not: every migrated production block path must construct a `DeviceDma`
capability and preallocate its descriptor storage before registration.
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

The implementation keeps these ownership boundaries visible in the source
layout:

- `ax-fs-ng::block::runtime::hctx::{submission}` separates the hctx event loop
  from batch collection/reconciliation;
- `ax-fs-ng::block::runtime::lifecycle::{controller,device,io}` separates
  controller transitions, installed queue/IRQ ownership, and filesystem-facing
  planned I/O windows;
- `ax-fs-ng::block::runtime::metrics` records dispatch batches, commit calls,
  terminal completions, largest batches, and peak in-flight depth without
  participating in synchronization;
- `ax-fs-ng::file::cache::{readahead,writeback,reclaim}` keeps page-cache read
  policy, durability boundaries, and memory-pressure eviction independent;
- `nvme-driver::block::{io_queue}` separates controller/IRQ setup from the
  queue-local CID, PRP, SQ, and CQ owner;
- `sdhci-host::host2::{bus,transaction}` separates physical transaction
  ownership from register-only bus-operation state machines,
  `sdhci-host::host::irq_state` isolates top-half state, while
  `sdhci-host::dma::{request}` separates ADMA2 ownership/recovery from
  descriptor policy; there is no FIFO module;
- `dwmmc-host::host2::{irq,bus,request,transaction}` separates event
  acknowledgement, register-only bus transitions, request ownership, and the
  Host2 adapter, while `dwmmc-host::dma::{idmac,request}` separates the reusable
  4 KiB descriptor ring from owned request state and `dwmmc-host::fifo`
  validates the SoC-supplied FIFO capability and derives Linux-compatible
  IDMAC thresholds;
- `phytium-mci-host::host2::{irq,bus,request,transaction}` applies the same
  ownership split to its protocol progression, while
  `phytium-mci-host::dma::{idmac,submission,completion}` isolates the
  controller-specific descriptor, DMA ownership transfer, recovery, and
  quarantine paths;
- `cv181x-sdhci::{platform,host2,board,clock}` separates resource/policy data,
  Host2 delegation, power/pad/PHY programming, and timing policy;
- `sdmmc-protocol::response::{card,identity,switch}` separates response bit
  decoding by protocol domain;
- `cv181x-sdhci::{board,clock}` separates TOP/pad/PHY programming from timing
  policy, while command, ADMA2, and IRQ state remain delegated to
  `sdhci-host`;
- `sdmmc-protocol::sdio::init::state_machine::{identify,mmc,sd}` separates the
  request's self-referential scratch ownership from protocol transition groups;
- SD/MMC tests follow the same domains (`init_flow`, `sd_speed`, `mmc_init`,
  `block_io_irq`, and `host2_adapter`) instead of one monolithic fixture file.

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

Only register-only initialization performed before scheduler availability may
use a bounded busy wait. Once the block runtime owns the controller,
`RegisterPending { retry_after }` is stored with its pending transition, reply,
and unified deadline. The maintenance task waits on its notification object
until the retry instant; IRQ and shutdown events take priority over the timer.
Command and data completions must transition through acknowledged IRQ events.

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
- `register_retry_after` and `advance_register_retry`, which may advance only
  register or protocol bookkeeping. The latter may publish a terminal result
  through its completion sink only when an earlier hard IRQ already
  acknowledged the hardware completion; it must never inspect a hardware
  completion source as a timer fallback;
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
filesystem's synchronous methods first build a bounded window of every request
required by the transfer planner, submit that window as one group, and only
then call `recv`. They are sleepable compatibility wrappers over the
asynchronous channel/IRQ core, not request-at-a-time hardware operations or
polling APIs.

Each hctx round-robins its mapped per-CPU channels and alternates preserved
partial-batch retries with fresh channel work. This keeps an accepted prefix
ordered without letting a persistent retry backlog starve newly submitted
tasks, including depth-one queues. A native batch is bounded by free tags,
queue depth, and `max_submit_batch`. A `QueueFull` suffix is retried without
reconstructing DMA. At most one normal commit is performed per batch operation.

Every explicit `CompletionGroup` shares one notification object. Completion
senders still publish distinct owned results, while the notification's pending
bit coalesces a burst into one scheduler activation.

The window follows Linux's all-submit-before-wait ownership rule used by
plugged multi-bio operations and direct I/O:

- [`block/fops.c`](https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/tree/block/fops.c?id=62cc90241548d5570ee68e01aaba6506964e9811)
  maps and submits the constituent bios before the synchronous path waits for
  the aggregate direct-I/O completion;
- [`block/blk-lib.c`](https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/tree/block/blk-lib.c?id=62cc90241548d5570ee68e01aaba6506964e9811)
  chains multiple bios under a plug and waits only after the construction loop;
- [`block/bio.c`](https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/tree/block/bio.c?id=62cc90241548d5570ee68e01aaba6506964e9811)
  keeps the synchronous wait wrapper separate from request construction.

TGOSKits bounds the caller-side window by the device's configured in-flight
limit and `max_submit_batch`. The selected hctx then clamps the native batch
again at dequeue time using its live free-tag count and queue depth. This
separation avoids exposing transient tag state to filesystem callers while
still preventing overcommit at the hardware boundary. The wrapper waits for
every submitted completion before returning the first terminal error, so an
early failure cannot release only part of the DMA ownership. It does not split
a request that already fits every hardware limit merely to manufacture a
larger batch.

### Page cache and durability

As in Linux, blk-mq does not invent file-level readahead or dirty writeback.
Those policies remain above the block runtime. The buffered page cache keeps a
per-file sequential-read state, resets it on a discontinuity, and grows a
contiguous backing-read window from four to at most 32 pages. A cache miss
performs one bounded backing read for the missing run; the backing block
adapter may then produce one large hardware request or a planned request group,
depending on hardware limits.

Dirty contiguous pages are snapshotted and written as runs. Ordinary ext4
`write_at`, append, and size changes leave rsext4 caches dirty instead of
forcing a device flush for every user write. `CachedFile::sync` first submits
and waits for all dirty runs, then invokes the inode `sync` boundary exactly
once so data, inode size, journal state, and the device flush are durable before
fsync returns. Close still does not imply fsync.

This follows the current Linux VFS split documented by
[`Documentation/filesystems/vfs.rst`](https://docs.kernel.org/filesystems/vfs.html)
and the page-cache operation model in
[`Documentation/filesystems/iomap/operations.rst`](https://docs.kernel.org/filesystems/iomap/operations.html):
readahead and writeback construct asynchronous I/O above the request queue,
while fsync forces and waits for dirty state at the durability boundary.

The hctx and controller tasks reuse event snapshot vectors. Adding or
registering a new IRQ endpoint may grow a snapshot once, but steady-state
submission, completion, and INTx rearm loops do not clone and allocate a fresh
IRQ-latch vector.

The IRQ event latch uses release/acquire publication and coalesces repeated
queue events. A maintenance task drains the latch before sleeping and the
notification primitive checks a pending bit while enqueueing the waiter, so an
IRQ between the final check and blocking cannot be lost.

A request watchdog first asks the controller state machine to stop DMA and
waits for the terminal register state. Only then may the queue return DMA and
fail subscriptions. If hardware shutdown cannot be confirmed, the runtime
quarantines the entire queue owner instead of dropping DMA-reachable memory.
The watchdog must not inspect a completion queue or resubmit a polling query.

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

All migrated SDHCI, DWMMC, and MCI queues advertise
`queue_depth = max_submit_batch = 1`. SDHCI selects 32-bit, 64-bit, or v4
ADMA2 descriptors only when both the capability registers and DMA mask permit
them, and splits at the 128 MiB boundary. DWMMC uses a preallocated 4 KiB ring
of 16-byte descriptors with at most 4 KiB payload per descriptor; neither one
descriptor nor one request may cross a 4 KiB DMA-address boundary. Its
advertised worst-case transfer limit reserves one descriptor for an unaligned
prefix. The IDMAC engine is reset before every descriptor-ring publication,
matching Linux `dw_mci_idmac_start_dma()`. Phytium uses a preallocated 4 KiB
ring of its 32-byte descriptor format and retains a 32-bit DMA mask until
hardware evidence supports a wider one. The runtime planner validates these
limits first; the driver validates again before moving DMA ownership.

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
- eMMC initialization parses `EXT_CSD_REV`, `CACHE_SIZE`, and `CACHE_CTRL`.
  When the card advertises a nonzero cache, the state machine enables it with
  IRQ-completed CMD6 before exposing the queue. Flush uses `FLUSH_CACHE` only
  while that cache is enabled. Cards without an enabled cache use an
  IRQ-completed CMD13 transfer-state barrier, rather than issuing an
  unsupported switch command or pretending that a polled completion occurred.
  This mirrors current Linux MMC cache enable/flush gating in
  [`drivers/mmc/core/mmc.c`](https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/tree/drivers/mmc/core/mmc.c?id=62cc90241548d5570ee68e01aaba6506964e9811).
- Generic SDHCI and CV181x wrappers use the same minimal SDHCI top half. DWMMC,
  JH7110, and Phytium handlers read/W1C controller and IDMAC status into an
  atomic mailbox. None of these handlers resets DMA, walks descriptors, takes
  a task lock, or completes a protocol request.
- JH7110 probe reads and validates `fifo-depth` from FDT, falling back to the
  known 32-word integration value only when the property is absent. A present
  malformed or unrepresentable value rejects probe. Linux uses
  `fifo-watermark-aligned` only for PIO interrupt thresholds, so the flag is
  retained for diagnostics but does not alter this IDMAC-only data path.

The hard IRQ path never drains a completion queue, allocates, copies DMA data,
or completes an OS request.

## Teardown and Failure

Teardown order is strict:

1. reject new channel submissions;
2. mask every device interrupt source;
3. disable and synchronize every IRQ registration;
4. free registrations, which drops boxed handlers after in-flight callbacks;
5. ask every hctx task to stop queue mutation while retaining SQ/CQ/ADMA memory;
6. disable the controller and wait for its terminal register state;
7. stop and join maintenance tasks;
8. return DMA and publish terminal failures for outstanding subscriptions;
9. release queue, controller, MMIO, and PCI resources.

Partial initialization and SMP expansion retain every emitted queue or started
hctx in the device transaction until controller shutdown is confirmed, then
use the same reverse-order unwind. A failed task spawn returns the queue to
that transaction instead of dropping its DMA memory.
MSI-X setup may be completely unwound and replaced by an explicitly selected
single-queue INTx mode. No error path selects polling.

## Performance Bounds and Follow-Ups

The runtime can create a native batch from concurrent callers or from one
filesystem transfer whose hardware-limit plan contains multiple requests.
Sequential buffered reads additionally amortize task wakeups through bounded
page-cache readahead. A single request that already fits the device limits
still produces one SQE; splitting it only to increase a batch counter would add
tags, PRPs, CQEs, and completion work without reducing doorbells.

Each committed window still has two intentional scheduling edges: hard IRQ to
the bound hctx task, then completion publication to the requesting task.
PREEMPT_RT has the analogous threaded/deferred IRQ handoff; improving that path
belongs in scheduler notification and remote-reschedule work, not in a driver
poll fallback.

`block_batch_stats()` exposes cumulative runtime batch, commit, terminal
completion, largest-batch, and peak-in-flight counters. They are relaxed
diagnostic snapshots and are not used to drive queue state.

Current bounds keep several linear operations small:

- NVMe queue depth and `max_submit_batch` are capped at 64. One acknowledged
  event drains at most those 64 CQ entries and writes one CQ-head doorbell.
  Linux batches completion-side tag release in groups of 32; before TGOSKits
  permits deeper queues, the queue contract should gain a typed “budget
  exhausted” continuation so the hctx can interleave submissions without
  rearming INTx early or polling.
- Pending-deadline selection scans at most the hctx queue depth. Replacing it
  with a deadline heap is justified only if queue depths grow materially.
- DWCMSHC is inherently depth one in this migration. It cannot gain native
  batching, and every command/data/STOP protocol phase needs an IRQ-to-hctx
  transition. Steady-state block data uses owned ADMA2 buffers; FIFO fallback
  is rejected by the RK3588 policy.
- CPU-to-hctx mapping uses the post-SMP online snapshot. CPU hotplug and sparse
  CPU-ID remapping remain unsupported and must be designed before either is
  enabled.

## Compatibility and Non-Goals

This is an intentional pre-1.0 breaking change. There is no compatibility
feature for borrowed requests or polling.

The first migration does not implement elevators, request merging, plugging,
I/O priorities, hotplug, multipath, multiple NVMe namespaces, discard, write
zeroes, or cross-hctx ordering other than the flush barrier.

QEMU root disks directly attached to ArceOS, StarryOS, or the Axvisor host use
NVMe. Guest-device ABIs that do not use this host block runtime are separate.

## Validation Matrix

Portable tests cover IRQ/top-half separation, lost-wakeup races, a synchronous
wrapper submitting its complete bounded request window before waiting,
single-request automatic grouping, one commit per batch, partial acceptance,
queue-full and fatal exits, commit failure, malformed driver ownership reports,
unexpected completion IDs, channel backpressure, dropped subscriptions,
out-of-order completion, flush barriers, DMA limits, timeout-without-polling,
registration teardown, adaptive bounded readahead, and batch-counter
accounting. NVMe pure-state tests prove that multiple staged SQ entries and
multiple consumed CQ entries each produce one publication until more work
arrives.

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
ADMA boundary splitting, fsync, and data-integrity verification. The board
helper is cross-compiled before the session, uploaded through the axbuild
session-file endpoint, and downloaded over the board-visible HTTP URL; the
validation does not mutate the persistent rootfs or depend on SSH.

The physical validation matrix and registration decision is:

| Controller / board | Public feature | Physical result |
| --- | --- | --- |
| CV181x SDHCI / LicheeRV Nano | `cv181x-sdhci` | Full read/write matrix and `SG2002_BLOCK_RW_BENCH_PASSED` |
| CV181x SDHCI / AKA-00 | `cv181x-sdhci` | Boot, controller, IRQ, and write matrix passed |
| JH7110 DWMMC / VisionFive2 | `starfive-jh7110-dwmmc` | Full write/fsync/checksum matrix and `VISIONFIVE2_BLOCK_RW_BENCH_PASSED` |
| RK3568 DWCMSHC eMMC | `rockchip-sdhci` | Strict compatible match and full eMMC matrix passed |
| RK3568 DWMMC SD/SDIO | none | Controller path booted, but no card was present for a write matrix |
| Phytium MCI / PhytiumPi | none | Controller/root media read succeeded; physical filesystem was read-only/corrupt |
| RK3588 DWCMSHC / OrangePi 5 Plus | `rockchip-sdhci` | Full matrix and `ORANGEPI_BLOCK_RW_BENCH_PASSED` |
| K230 | none | Compile/static audit only until hardware and upstream evidence exist |

Each physical row checks the root device, DMA domain/mask, nonzero IRQ, stable
idle IRQ count, equal submission/completion counts, and zero pending or
quarantined requests. Session payloads use axbuild `session_files` and
`${sessionFile:...}` HTTP URLs; SSH and rsync are not part of this workflow.

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

### 2026-07-28 post-rebase x86_64 execution record

After rebasing onto `origin/dev` commit `99d0965f9`, the baseline and refactored
runtime were rerun with eight CPUs, 512 MiB memory, an NVMe controller advertising
`max_ioqpairs=64,msix_qsize=65`, five 4-MiB rounds, 4-KiB operations, and fsync.
Both used the same rootfs archive and benchmark sources, but separate
axbuild-managed image copies; therefore these numbers are an execution record,
not a claim of a controlled performance improvement.

| Revision and effective mode | Write | Read | Result |
| --- | ---: | ---: | --- |
| `99d0965f9` (`origin/dev`), legacy INTx | 4,147,174 us / 0.96 MiB/s | 214,756 us / 18.62 MiB/s | checksum `2673868800`, passed |
| rebased blk-mq working tree, 8 MSI-X I/O hctxs | 8,830,008 us / 0.45 MiB/s | 343,871 us / 11.63 MiB/s | checksum `2673868800`, passed |

The refactored result is 53.1% lower for write throughput and 37.5% lower for
read throughput in this workload. Both runs include the same latest scheduler
remote-reschedule changes, so that change alone does not remove the regression.
The benchmark issues synchronous filesystem operations from one task, which
normally leaves no concurrent requests for runtime auto-batching; it primarily
measures the extra hard-IRQ-to-hctx and hctx-to-requester wakeup path rather than
NVMe batch or multi-queue scaling. This remains a recorded follow-up for task
wakeup and concurrent-workload analysis, not a reason to reintroduce polling.

Explicit, reusable x86_64 configurations exercise the full MSI-X SMP matrix:

| Online CPUs / hctxs | Write median | Read median | Result |
| ---: | ---: | ---: | --- |
| 1 / 1 | 13,534,424 us / 0.29 MiB/s | 511,989 us / 7.81 MiB/s | checksum `2673868800`, passed |
| 2 / 2 | 11,983,815 us / 0.33 MiB/s | 444,114 us / 9.00 MiB/s | checksum `2673868800`, passed |
| 4 / 4 | 9,891,941 us / 0.40 MiB/s | 395,377 us / 10.11 MiB/s | checksum `2673868800`, passed |
| 8 / 8 | 9,913,460 us / 0.40 MiB/s | 374,743 us / 10.67 MiB/s | checksum `2673868800`, passed |

The post-rebase non-x86 StarryOS NVMe runs use the same five-round workload
and checksum:

| Architecture | Online CPUs / hctxs | IRQ mode | Write median | Read median | Result |
| --- | ---: | --- | ---: | ---: | --- |
| aarch64 | 4 / 4 | MSI-X | 16,415,791 us / 0.24 MiB/s | 569,022 us / 7.02 MiB/s | checksum `2673868800`, passed |
| riscv64 | 4 / 1 | INTx, selected at initialization | 15,466,727 us / 0.25 MiB/s | 563,799 us / 7.09 MiB/s | checksum `2673868800`, passed |
| loongarch64 | 4 / 1 | PCH-PIC INTx, selected at initialization | 12,656,132 us / 0.31 MiB/s | 483,255 us / 8.27 MiB/s | checksum `2673868800`, passed |

The RISC-V run booted on physical hart 3 and verified that the rebased CPU
topology maps that boot hart to logical CPU 0 before the block runtime brings
the remaining CPUs and submission channels online.

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

### 2026-07-28 windowed-I/O execution record

After the synchronous filesystem wrappers were changed to submit a bounded
request window before sleeping, the same five-round QEMU benchmark completed
on all supported QEMU architectures:

| Architecture | Online CPUs / hctxs | IRQ mode | Write | Read | Result |
| --- | ---: | --- | ---: | ---: | --- |
| x86_64 | 8 / 8 | MSI-X | 1,443,500 us / 2.77 MiB/s | 92,175 us / 43.39 MiB/s | checksum `2673868800`, passed |
| aarch64 | 4 / 4 | MSI-X | 2,427,046 us / 1.64 MiB/s | 126,684 us / 31.57 MiB/s | checksum `2673868800`, passed |
| riscv64 | 4 / 1 | INTx | 2,107,364 us / 1.89 MiB/s | 117,146 us / 34.14 MiB/s | checksum `2673868800`, passed |
| loongarch64 | 4 / 1 | PCH-PIC INTx | 1,843,057 us / 2.17 MiB/s | 94,998 us / 42.10 MiB/s | checksum `2673868800`, passed |

These measurements also include the page-cache readahead and ext4 durability
boundary changes, so they are validation records rather than a single-cause
performance claim.

The subsequent OrangePi 5 Plus run obtained its helper through the axbuild
session HTTP endpoint, enabled the card's advertised 65,536-KiB volatile cache
through the IRQ-driven initialization state machine, and emitted the success
marker after all five cases:

| Case | I/O size | Data | Write | Read | fsync |
| --- | ---: | ---: | ---: | ---: | ---: |
| sector | 512 B | 8 MiB | 0.40 MiB/s | 19.04 MiB/s | 71 ms |
| page | 4 KiB | 8 MiB | 1.39 MiB/s | 19.25 MiB/s | 71 ms |
| ADMA maximum | 1,048,448 B | 8 MiB | 2.16 MiB/s | 19.58 MiB/s | 78 ms |
| ADMA boundary split | 1,048,960 B | 8 MiB | 2.17 MiB/s | 19.18 MiB/s | 78 ms |
| eight-task concurrent | 4 KiB | 2 MiB/task | 5,801 ms elapsed | verified | fsync/task |

The depth-one eMMC queue intentionally cannot perform a native multi-command
hardware batch; the per-CPU channels still provide backpressure and fair
serialization through its single hctx.

### 2026-07-29 VisionFive2 execution record

The VisionFive2 SD root filesystem was repaired from the board's default Linux
environment before destructive testing. StarryOS then booted four CPUs from the
same FDT, read `fifo-depth = 32` for both JH7110 controllers, selected the SD
controller's one depth-one IDMAC hctx, mounted ext4 read/write, and completed
the serial-inline matrix:

| Case | I/O size | Data | Write | Result |
| --- | ---: | ---: | ---: | --- |
| sector | 512 B | 2,097,152 B | 797.9 kB/s | fsync and checksum passed |
| page | 4 KiB | 2,097,152 B | 1.3 MB/s | fsync and checksum passed |
| IDMAC maximum | 1,044,480 B | 5,222,400 B | 846.6 kB/s | fsync and checksum passed |
| planner split | 1,044,992 B | 5,224,960 B | 687.8 kB/s | fsync and checksum passed |
| eight-task concurrent | 4 KiB | 512 KiB/task | 1 s timer granularity | fsync/task and checksum passed |

The run emitted `VISIONFIVE2_BLOCK_RW_BENCH_PASSED`. These figures are a
functional execution record; scheduler wakeup latency remains outside the
driver and is not hidden with polling.
