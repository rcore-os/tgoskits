---
name: cross-kernel-driver
description: Create, refactor, review, and optimize portable Rust driver crates under `drivers/` by device type in this tgoskits workspace. Use this skill when adding or changing cross-Rust-kernel drivers, separating Driver Core / Capability Boundary / OS Glue / Runtime layers, handling MMIO/iomap with `mmio-api`, handling DMA with `dma-api`, designing IRQ callback ownership, control/IRQ/queue endpoint contracts, queue-local completion state, or auditing OS API coupling in driver code.
---

# Cross Kernel Driver

## Overview

Use this skill to keep reusable driver crates portable across Rust kernels by separating stable hardware logic from OS API coupling. The target shape is: Driver Core owns registers, descriptors, bounded state machines, queues, and events; Capability Boundary owns MMIO, DMA, IRQ, queue, and typed ownership contracts; OS Glue owns probe, iomap/remap, IRQ registration, and task scheduling; Runtime owns CPU-pinned maintenance domains, blocking facades, request tables, and recovery orchestration. Generic shared workqueues remain available for unrelated short work, but one stateful IRQ device is advanced by exactly one non-migratable maintenance thread. For IRQ-driven devices, prefer an explicit runtime split into control, IRQ handler, and queue endpoints so each endpoint has one clear owner and synchronization contract.

For nontrivial driver design or refactoring, read `references/architecture.md` before editing.

## Workflow

1. Inspect the requested device, existing `drivers/` crates, root `Cargo.toml`, and any platform glue under `platforms/axplat-dyn/src/drivers`.
2. Place reusable hardware/IP crates under `drivers/<device-type>/...`; add a vendor/family subdirectory when it matches the existing type layout or avoids ambiguity.
3. Keep `src/` OS independent. Put target-kernel glue, FDT/probe code, board setup, `iomap`, IRQ registration, and OS wakeups in `tests/`, examples, platform glue, or adapter crates.
4. Add new driver crates to workspace `members` and `[workspace.dependencies]` when they are meant to be consumed by this repo.
5. For ArceOS/dynamic-platform integration, keep adapters in the existing platform module names such as `platforms/axplat-dyn/src/drivers/blk`, even if the reusable crate lives under `drivers/block`.
6. Use small capability traits or API objects instead of a monolithic `KernelHal`. Split MMIO, DMA, IRQ event, queue contract, and wake/poll boundaries.
7. Model queues as independent running units. For block I/O, transfer an owned request with `submit_owned`, consume only IRQ-produced event batches with `service_events`, and return retained ownership only after a typed DMA-quiescence proof. Hardware queue methods are called only by their maintenance owner; remote submitters publish owned requests to its ingress mailbox.
8. For IRQ-driven devices, keep control, IRQ handler, and queue endpoints separate. The maintenance thread owns startup/config/service and queue operations; the registered IRQ callback owns the IRQ endpoint; non-owner callers see only request/completion facades.
9. Move lifetime-sensitive IRQ handler endpoints into the registered IRQ callback. The maintenance thread must register its own callback after pinning itself and the IRQ affinity to the same CPU. Do not create an action on one CPU for a worker on another CPU, and do not share the handler through a remotely callable `Arc<Mutex<_>>`.
10. IRQ handlers acknowledge and snapshot hardware events into a preallocated maintenance mailbox, then use a saved local IRQ wake for their owner thread. IRQ event and fault values must be plain `Copy` data with no destructor or hidden allocation ownership. The owner advances requests without re-reading shared/destructive IRQ status. Completion timers are watchdogs, never polling fallbacks.
11. Make IRQ paths implement `IrqEndpoint::capture` and `contain`. Distinguish an unhandled shared line, an acknowledged stable event, and a contained or uncontained fault. Lock contention is an ownership bug, not an event and not a deferred acknowledgement request.
12. When IRQ and owner-task paths share mutable register state, keep both on one CPU. Owner-task mutation briefly disables local IRQs; the hard handler touches only the pre-registered stable object. Document this exclusion contract. Other CPUs communicate by mailbox and ordinary scheduler wake/IPI, never by synchronous register-owner calls.
13. Validate the changed crate with formatting and targeted clippy before finishing.

For exclusive passthrough, disabling an OS IRQ action is not ownership
transfer. Mask the device, drain the action, remove it from the descriptor while
retaining its callback in a linear token, and only then publish guest ownership.
After guest routes are revoked, reattach the host token disabled before running
the IRQ-capable reinitialization state machine. Do not retain a dormant host
action or relax share/affinity compatibility to make the guest registration fit.

## Dependency Rules

- Keep `[dependencies]` free of OS-specific crates in reusable driver crates.
- Put OS-specific test/runtime crates in `[dev-dependencies]` unless the crate is explicitly OS Glue.
- Prefer `foo.workspace = true` for dependencies already declared in root `[workspace.dependencies]`.
- Prefer the latest `mmio-api` for MMIO/iomap boundaries and the latest `dma-api` for DMA boundaries. As of 2026-04-28, crates.io reported `mmio-api = "0.2.1"` and `dma-api = "0.7.2"`; re-check with `cargo search` or `cargo info` before bumping versions.
- This workspace already has `dma-api` in root `[workspace.dependencies]`. If MMIO support is added broadly, add `mmio-api` there and consume it with `workspace = true`.

## MMIO/IOMAP

- Do not call raw OS `ioremap`/`iomap` helpers from portable driver core code.
- Implement or use `mmio_api::MmioOp` in OS Glue. Keep `ioremap`, `iounmap`, mapping failure handling, and mapping lifetime there.
- Pass already-mapped MMIO into Driver Core as `mmio_api::Mmio`, `mmio_api::MmioRaw`, `NonNull<u8>`, or a typed register wrapper, following nearby crate style.
- Keep unsafe pointer construction near the MMIO boundary with an explicit safety contract.

## DMA

- Treat DMA as a capability, not allocation sugar.
- Let OS Glue implement `dma_api::DmaOp`; create `dma_api::DeviceDma::new(dma_mask, &impl)` for the device.
- In Driver Core, prefer `dma-api` containers and handles such as `DArray`, `DBox`, `SArrayPtr`, `DmaDirection`, `DmaAddr`, `DmaHandle`, and `DmaMapHandle` rather than ad hoc bus-address bookkeeping.
- Always handle DMA mask/address width, alignment, cache sync direction, ownership/lifetime, zero-copy transfer ownership, and bus address vs CPU virtual address.

## Interface Shape

Use `&mut self` APIs where exclusive access is the natural contract. Do not require callers to provide an OS lock as part of the portable abstraction. If only the IRQ callback should call a handler, make that visible in the type shape: move the handler into the callback and expose `handle(&mut self, ...)` instead of making the handler a clonable shared object.

For block-device integration in ArceOS, expose portable block drivers through `rdif_block::Interface` and its owned queue API. Keep queue creation, tags, DMA wait policy, maintenance threads, watchdogs, and IRQ registration in `ax-runtime`; the portable boundary transfers `OwnedRequest` values and exposes an IRQ-owned `rdif_irq::IrqEndpoint`. Hardware discovery must not issue reset/identify commands: expose a pending controller initialization endpoint and let the final maintenance owner bind IRQ actions before advancing the bounded initialization FSM. Capacity and queues are published only after `Ready`.

Prefer small interfaces:

```rust
pub trait IrqEndpoint {
    type Event;
    type Fault;

    fn capture(&mut self) -> IrqCapture<Self::Event, Self::Fault>;
    fn contain(
        &mut self,
        cause: ContainmentCause,
    ) -> Result<MaskedSource, Self::Fault>;
}

pub trait IQueue {
    fn id(&self) -> usize;
    fn submit_owned(
        &mut self,
        id: RequestId,
        request: OwnedRequest,
    ) -> Result<SubmitOutcome, SubmitError>;
    fn service_events(
        &mut self,
        events: &QueueEventBatch<'_>,
        sink: &mut dyn CompletionSink,
    ) -> Result<ServiceProgress, BlkError>;
}
```

IRQ handlers should identify/clear the interrupt source and extract a stable event. They should not allocate, block, run slow paths, or hold broad locks. If the endpoint cannot capture safely, it must mask the precise device source or report an uncontained fault so OS glue can fail closed. Keep the principle visible during reviews: "interrupts capture state; one bounded owner thread advances flow" (`中断只捕获状态，单一有界维护线程推进流程`).

For runtime designs with richer state, prefer returning split parts:

```rust
pub struct DeviceParts {
    pub control: ControlPort,
    pub irq: IrqHandler,
    pub queues: QueueSet,
}
```

Move the complete parts into the selected maintenance thread. That thread registers `irq` into the callback on its own CPU, then exclusively owns `control` and queue endpoints until explicit close.

For a composite network device such as AIC-over-SDIO, do not create a firmware, RX, TX, or "AIC maintenance" thread inside the portable driver crate. The portable aggregate owner retains the bus transport, firmware/device state machines, queue endpoints, and IRQ capture endpoint as one physical-controller bundle. OS glue moves that bundle into one generic network maintenance domain whose fixed owner registers the host/controller IRQ. Firmware initialization and queue service run as bounded transitions in that same owner; they do not embed an OS waker, task API, or workqueue in the driver.

The runtime's move-only IRQ action token must participate in maintenance close accounting even when its callback captures no wake capability. Detach and reattach transfer the same live count; only successful explicit close releases it. Dropping an active or detached token without close is fail-closed quarantine, not an implicit teardown or permission to release the owner's CPU lease.

When a driver intentionally shares registries or queue maps between task setup and IRQ completion paths, prefer an xHCI-style exclusion protocol over taking the same spinlock in IRQ: task context masks the same device interrupter/MSI source before mutation; IRQ context does not take that lock and only touches entries whose lifetime was established before interrupts were enabled. This avoids same-lock IRQ reentry deadlocks, but it does not make allocation, blocking, arbitrary wakers, or unrelated OS callbacks safe in hard IRQ.

For split queue designs, do not make an IRQ handler lock a queue mutex that task context can hold. If IRQ and queues share one hardware register block, put exclusive register access behind one same-CPU, non-blocking owner/gate and let the IRQ endpoint be the sole reader/clearer of shared or destructive IRQ status. The callback publishes only a stable event; the maintenance thread alone fans it into per-queue completion state. `service_events` consumes that snapshot and must never become a hidden completion poll.

## Validation

Run:

```bash
cargo fmt
cargo xtask clippy --package <crate>
```

If platform glue changes, also run:

```bash
cargo xtask clippy --package axplat-dyn
```

If a generic ArceOS adapter changes, also run the matching package, for example:

```bash
cargo xtask clippy --package ax-driver-net
```

When a driver crate now passes clippy and is missing from `scripts/test/clippy_crates.csv`, add it in the same change.

Board or bare-metal tests in `drivers/*/tests` may require crate-local runners or real hardware; treat them as target-specific validation, not default CI-safe checks.

## References

- `references/architecture.md`: detailed architecture rules derived from `target/跨rust kernel的驱动架构设计.md`, `target/跨rust kernel的驱动架构设计v3.pptx`, and current repo driver conventions.
