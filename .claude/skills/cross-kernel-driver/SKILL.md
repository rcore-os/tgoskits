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
7. Model queues as independent running units. For block I/O, transfer an owned request with `submit_owned`, consume one linear IRQ-evidence identity at a time, and return retained ownership only after a typed DMA-quiescence proof. Hardware queue methods are called only by their maintenance owner; remote submitters publish owned requests to its software-context ingress.
8. For IRQ-driven devices, keep control, IRQ handler, and queue endpoints separate. The maintenance thread owns startup/config/service and queue operations; the registered IRQ callback owns the IRQ endpoint; non-owner callers see only request/completion facades.
9. Move lifetime-sensitive IRQ handler endpoints into the registered IRQ callback. The maintenance thread must register its own callback after pinning itself and the IRQ affinity to the same CPU. Do not create an action on one CPU for a worker on another CPU, and do not share the handler through a remotely callable `Arc<Mutex<_>>`.
10. IRQ handlers acknowledge hardware facts into a preallocated driver ledger, publish only its opaque evidence identity into the maintenance mailbox, then use a saved local IRQ wake for their owner thread. A source may have only one outstanding evidence owner: repeated capture of the same ledger identity coalesces a dirty/rerun fact and must not mint another `PendingBlockIrq`; a different identity while the old one is live is a containment fault. An evidence owner may be retained, drained exactly once, or transferred into recovery; only drained evidence can authorize source rearm. Clearing the source latch uses clear-and-recheck so a capture racing the drain cannot be lost. The owner advances requests without re-reading shared/destructive IRQ status. Completion timers are watchdogs, never polling fallbacks.
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

For block-device integration in ArceOS, expose interrupt-backed portable drivers through the staged `rdif-block` activation boundary. Discovery reports immutable controller identity, ownership-domain topology, IRQ capabilities, and constraints only; it must not fabricate namespace geometry that becomes known after Identify. `ax-runtime` selects one immutable plan, moves the prepared control part into its final CPU owner, binds the IRQ actions there, and only then advances the bounded initialization session with linear IRQ evidence. Final queue parts, capacity, block size, and logical-device routes are published atomically only after `Ready`. Inline devices use the separate `InlineExecuteQueue` boundary and return ownership in the same call.

`Ready` is not permission to collect all queue objects back into one central controller. Stage publication into a pure catalog/coordinator plus move-only unbound I/O domains. Each domain is transferred to its final maintenance owner, binds its exact sources there, becomes `!Send`, and returns only a move-only binding proof. The coordinator publishes routes and geometry after consuming one proof for every domain. Hardware depth belongs to the ownership domain/hctx activation plan and realized queue descriptor, never to a logical namespace. Portable source identities are unique between I/O domains; physical shared lines are represented by OS bindings, while a shared control owner may name only the exact source subset of its I/O domain.

When the control capability is `SharedWithIo`, initialization and normal I/O are two phases of the same maintenance owner, not two threads. If the hardware control and queue state can be moved into disjoint owners (for example independent NVMe queue storage), a staged `AlreadyBound` I/O part may remain separate. If the same command engine or register owner is needed for initialization, I/O, and recovery (for example SD/MMC), use the combined shared-domain boundary: the ready proof contains only immutable queue facts, while the original control object remains the sole driver owner and lends its I/O capability through `&mut self` in the same maintenance session. Never simulate this combined owner with `Arc<UnsafeCell<_>>`, a broad lock, or two simultaneously callable trait objects. Only domains with new, independently owned sources move to new maintenance owners. Re-registering a shared source or moving its queue after `Ready` violates the ownership-domain contract.

A plan-selected physical queue may exist even when initialization discovers no logical device behind it, as with an empty AHCI port. Publish that queue explicitly as `LogicalDeviceSelector::Unrouted`; do not fabricate a namespace, omit a selected hardware owner, or collapse independent physical queues into one controller-wide serialized queue. `Unrouted` is a final queue-routing fact only and is invalid for an ownership-domain capability, which must still describe the devices it can potentially serve.

Platform discovery resources are one linear transaction as well. Keep the immutable binding facts, parent IRQ allocation lease, and portable activator behind private fields from discovery through `Prepared`, `Staged`, and `Published` owners. A transition failure must return the complete retained transaction; callers must not destructure the lease from the driver owner. `Staged` is consumed once into a publication owner plus unbound domains, rather than exposing a repeatable `take_*` method that leaves an ambiguous half-empty state.

Prefer small interfaces:

```rust
pub trait IrqEndpoint {
    type Fault;

    fn capture(&mut self) -> IrqCapture<IrqEvidenceId, Self::Fault>;
    fn contain(
        &mut self,
        cause: ContainmentCause,
    ) -> Result<MaskedSource, Self::Fault>;
}

pub trait InterruptIoDomain {
    fn submit_owned(
        &mut self,
        queue: HardwareQueueId,
        device: LogicalDeviceId,
        id: RequestId,
        request: OwnedRequest,
    ) -> Result<AcceptedRequest, UnacceptedRequest>;
    fn service_irq(&mut self, evidence: PendingBlockIrq) -> IrqServiceDecision;
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

For split queue designs, do not make an IRQ handler lock a queue mutex that task context can hold. If IRQ and queues share one hardware register block, put exclusive register access behind one same-CPU, non-blocking owner/gate and let the IRQ endpoint be the sole reader/clearer of shared or destructive IRQ status. The callback publishes only an opaque identity for facts already stored in the driver ledger; the maintenance thread consumes that identity once and must never turn evidence service into a hidden completion poll.

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
