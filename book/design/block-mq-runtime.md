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

The software/hardware queue split follows Linux blk-mq as documented and
implemented in Linux v7.2-rc4:

- a CPU-local software submission context selects a hardware context;
- a hardware context owns tags and the device submission/completion queue;
- completion ordering is not implied across hardware contexts;
- queue limits are enforced before dispatch.

The interrupt split follows the PREEMPT_RT primary-handler rule: hard IRQ work
is restricted to source identification, device acknowledgement or masking,
publishing preallocated state, and activating deferred task context. TGOSKits
uses dedicated maintenance tasks rather than importing Linux threaded-IRQ or
softirq infrastructure.

Hardware behavior is checked against the NVMe base specification and the
RK3588 DWCMSHC/SDHCI programming model used by the corresponding Linux driver.
The implementation PR must record the exact specification revisions and Linux
source commit used for register-level decisions.

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

- `submit_owned`, which transfers an `OwnedRequest` and its prepared DMA to
  hardware and returns a tag;
- `drain_completions`, which is called only after a matching IRQ event and
  transfers terminal results and completed DMA to a sink;
- `shutdown`, which quiesces hardware and returns or quarantines every remaining
  DMA allocation.

There is no request polling or cancellation interface. Dropping a caller's
completion subscription does not cancel hardware I/O.

## Runtime Data Flow

```text
caller
  -> per-CPU bounded submission channel
  -> mapped hctx maintenance task
  -> exclusively owned HardwareQueue

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
entering hardware. A completion subscription provides blocking `recv` only.
The filesystem's synchronous methods are wrappers over submit plus `recv`; they
are not polling APIs.

The IRQ event latch uses release/acquire publication and coalesces repeated
queue events. A maintenance task drains the latch before sleeping and the
notification primitive checks a pending bit while enqueueing the waiter, so an
IRQ between the final check and blocking cannot be lost.

A request watchdog may fail and reset a timed-out device. It must not inspect a
completion queue or resubmit a polling query.

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
- maximum blocks per request;
- maximum segment count and segment length;
- an optional power-of-two boundary that no segment may cross;
- supported operations and request flags.

The transfer planner splits at block, byte, segment-count, segment-size, and
boundary limits. Validation rejects overflow, mask violations, misalignment,
unsupported flags, and buffers that cannot be represented. The hardware driver
repeats validation immediately before taking DMA ownership.

NVMe derives limits from CAP, controller page size, namespace LBA size, MDTS,
PRP-list capacity, DMA mask, and queue depth. RK3588 derives limits from SDHCI
block count, ADMA2 descriptor length/address rules, and its 32-bit DMA mask.

## Interrupt Details

IRQ actions use non-reentrant execution, fixed affinity, and disabled-at-register
semantics.

- NVMe MSI-X has one admin vector and one vector per I/O hctx. A dedicated
  vector publishes its fixed queue bit; controller EOI completes delivery.
- NVMe INTx verifies that the PCI function asserts INTx, masks vector zero with
  `INTMS`, and returns `MaskedNeedsRearm`. The worker drains the CQ, updates the
  CQ head, and unmasks with `INTMC`.
- DWCMSHC reads normal/error status, performs the required W1C acknowledgement,
  and publishes command/data/error bits. The maintenance task advances the
  SD/MMC protocol state machine.

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

Portable tests cover IRQ/top-half separation, lost-wakeup races, channel
backpressure, dropped subscriptions, out-of-order completion, flush barriers,
DMA limits, timeout-without-polling, and registration teardown.

QEMU validation covers:

- x86_64 with 1, 2, 4, and 8 CPUs in MSI-X mode;
- x86_64 with one advertised MSI-X vector to exercise INTx;
- NVMe root filesystem read/write on aarch64, riscv64, and loongarch64;
- ArceOS, StarryOS, and Axvisor-host root filesystem boot paths.

OrangePi 5 Plus validation covers eight CPUs, DWCMSHC eMMC root discovery,
concurrent block read/write, 512-byte and 4-KiB requests, maximum transfers,
ADMA boundary splitting, fsync, and data-integrity verification.
