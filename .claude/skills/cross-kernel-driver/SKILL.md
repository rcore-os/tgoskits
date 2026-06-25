---
name: cross-kernel-driver
description: Create, refactor, review, and optimize portable Rust driver crates under `drivers/` by device type in this tgoskits workspace. Use this skill when adding or changing cross-Rust-kernel drivers, separating Driver Core / Capability Boundary / OS Glue / Runtime layers, handling MMIO/iomap with `mmio-api`, handling DMA with `dma-api`, designing IRQ callback ownership, control/IRQ/queue endpoint contracts, queue-local completion state, or auditing OS API coupling in driver code.
---

# Cross Kernel Driver

## Overview

Use this skill to keep reusable driver crates portable across Rust kernels by separating stable hardware logic from OS API coupling. The target shape is: Driver Core owns registers, descriptors, state machines, queues, and events; Capability Boundary owns MMIO, DMA, IRQ, and queue contracts; OS Glue owns probe, iomap/remap, IRQ registration, and task scheduling; Runtime owns blocking / poll / future / worker integration. For IRQ-driven devices, prefer an explicit runtime split into control, IRQ handler, and queue endpoints so each endpoint has one clear owner and synchronization contract.

For nontrivial driver design or refactoring, read `references/architecture.md` before editing.

## Workflow

1. Inspect the requested device, existing `drivers/` crates, root `Cargo.toml`, and any platform glue under `platform/axplat-dyn/src/drivers`.
2. Place reusable hardware/IP crates under `drivers/<device-type>/...`; add a vendor/family subdirectory when it matches the existing type layout or avoids ambiguity.
3. Keep `src/` OS independent. Put target-kernel glue, FDT/probe code, board setup, `iomap`, IRQ registration, and OS wakeups in `tests/`, examples, platform glue, or adapter crates.
4. Add new driver crates to workspace `members` and `[workspace.dependencies]` when they are meant to be consumed by this repo.
5. For ArceOS/dynamic-platform integration, keep adapters in the existing platform module names such as `platform/axplat-dyn/src/drivers/blk`, even if the reusable crate lives under `drivers/block`.
6. Use small capability traits or API objects instead of a monolithic `KernelHal`. Split MMIO, DMA, IRQ event, queue contract, and wake/poll boundaries.
7. Model queues as independent running units. Prefer APIs such as `submit`, `reclaim`, `poll`, `submit_request`, and `poll_request`.
8. For IRQ-driven devices, keep control, IRQ handler, and queue endpoints separate. The control endpoint owns startup/config/service operations; the IRQ endpoint synchronizes hardware events; queue endpoints submit/reclaim work using queue-local state.
9. Move lifetime-sensitive IRQ handler endpoints into the registered IRQ callback when possible. Prefer `FnMut`/boxed callback ownership or an equivalent OS registration token over sharing the IRQ handler through `Arc<Mutex<_>>`.
10. IRQ handlers should synchronize hardware events into queue-local completion state; queues should advance their own work without locking the IRQ handler or re-reading shared/destructive IRQ status.
11. Make IRQ paths return stable events, normally `handle_irq(&mut self) -> Event` for an IRQ-owned endpoint or `handle_irq() -> Event` for stateless/raw event extractors. OS Glue decides whether to wake a thread, wake a future, schedule a worker, or set a pending flag.
12. When IRQ and task paths share mutable driver state, look for an explicit exclusion protocol: task-side mutation masks the exact interrupt source before taking the lock, while IRQ only touches pre-registered stable state. Document the lifetime/safety contract; otherwise prefer atomics/pending bits plus a deferred worker.
13. Validate the changed crate with formatting and targeted clippy before finishing.

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

For block-device integration in ArceOS, expose portable block drivers through `rdif_block::Interface` and `rdif_block::IQueue`. Keep queue creation, DMA/wait policy, and IRQ registration in OS glue/runtime layers; the portable boundary should be submit/poll requests plus `handle_irq() -> Event`.

Prefer small interfaces:

```rust
pub trait IrqHandle {
    fn handle_irq(&mut self) -> Event;
}

pub trait IQueue {
    fn id(&self) -> usize;
    fn submit_request(&mut self, req: Request<'_>) -> Result<RequestId, Error>;
    fn poll_request(&mut self, id: RequestId) -> Result<(), Error>;
}
```

IRQ handlers should identify/clear the interrupt source and extract an `Event`. They should not block, run long slow paths, or hold broad locks. Keep the principle visible during reviews: "interrupts synchronize state; tasks advance flow" (`中断只同步状态，任务才推进流程`).

For runtime designs with richer state, prefer returning split parts:

```rust
pub struct DeviceParts {
    pub control: Arc<ControlPort>,
    pub irq: IrqHandler,
    pub queues: QueueSet,
}
```

Register `irq` by moving it into the OS IRQ callback. Let task/worker code hold `control` and queue endpoints, not the IRQ handler itself.

When a driver intentionally shares registries or queue maps between task setup and IRQ completion paths, prefer an xHCI-style exclusion protocol over taking the same spinlock in IRQ: task context masks the same device interrupter/MSI source before mutation; IRQ context does not take that lock and only touches entries whose lifetime was established before interrupts were enabled. This avoids same-lock IRQ reentry deadlocks, but it does not make allocation, blocking, arbitrary wakers, or unrelated OS callbacks safe in hard IRQ.

For split queue designs, do not make an IRQ handler lock a queue mutex that task context can hold. If IRQ and queues share one hardware register block, put exclusive register access behind one short, non-blocking core/gate, let the IRQ endpoint be the sole reader/clearer of shared or destructive IRQ status, and fan out results into independent per-queue completion state. Queue `poll` should normally mean "consume synchronized completion state", not "peek the global IRQ/status register again".

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
