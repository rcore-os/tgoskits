---
name: cross-kernel-driver
description: Create, refactor, review, and optimize portable Rust driver crates under `drivers/` by device type in this tgoskits workspace. Use this skill when adding or changing cross-Rust-kernel drivers, separating Driver Core / Capability Boundary / OS Glue / Runtime layers, handling MMIO/iomap with `mmio-api`, handling DMA with `dma-api`, designing IRQ event or queue contracts, or auditing OS API coupling in driver code.
---

# Cross Kernel Driver

## Overview

Use this skill to keep reusable driver crates portable across Rust kernels by separating stable hardware logic from OS API coupling. The target shape is: Driver Core owns registers, descriptors, state machines, queues, and events; Capability Boundary owns MMIO, DMA, IRQ, and queue contracts; OS Glue owns probe, iomap/remap, IRQ registration, and task scheduling; Runtime owns blocking / poll / future / worker integration.

For nontrivial driver design or refactoring, read `references/architecture.md` before editing.

## Workflow

1. Inspect the requested device, existing `drivers/` crates, root `Cargo.toml`, and any platform glue under `platform/axplat-dyn/src/drivers`.
2. Place reusable hardware/IP crates under `drivers/<device-type>/...`; add a vendor/family subdirectory when it matches the existing type layout or avoids ambiguity.
3. Keep `src/` OS independent. Put target-kernel glue, FDT/probe code, board setup, `iomap`, IRQ registration, and OS wakeups in `tests/`, examples, platform glue, or adapter crates.
4. Add new driver crates to workspace `members` and `[workspace.dependencies]` when they are meant to be consumed by this repo.
5. For ArceOS/dynamic-platform integration, keep adapters in the existing platform module names such as `platform/axplat-dyn/src/drivers/blk`, even if the reusable crate lives under `drivers/block`.
6. Use small capability traits or API objects instead of a monolithic `KernelHal`. Split MMIO, DMA, IRQ event, queue contract, and wake/poll boundaries.
7. Model queues as independent running units. Prefer APIs such as `submit`, `reclaim`, `poll`, `submit_request`, and `poll_request`.
8. Make IRQ paths return stable events, normally `handle_irq() -> Event`. OS Glue decides whether to wake a thread, wake a future, schedule a worker, or set a pending flag.
9. Validate the changed crate with formatting and targeted clippy before finishing.

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

Use `&mut self` APIs where exclusive access is the natural contract. Do not require callers to provide an OS lock as part of the portable abstraction.

For block-device integration in `axplat-dyn`, wrap the portable driver with `rd_block::Interface` and `rd_block::IQueue`, then register it through the existing `PlatformDeviceBlock::register_block` path. Keep `rd-block`/`rdrive` adapter code in platform glue or behind an explicit adapter feature unless the crate's purpose is to expose that interface.

Prefer small interfaces:

```rust
pub trait IrqHandle {
    fn handle_irq(&self) -> Event;
}

pub trait IQueue {
    fn id(&self) -> usize;
    fn submit_request(&mut self, req: Request<'_>) -> Result<RequestId, Error>;
    fn poll_request(&mut self, id: RequestId) -> Result<(), Error>;
}
```

IRQ handlers should identify/clear the interrupt source and extract an `Event`. They should not block, run long slow paths, or hold broad locks. Keep the principle visible during reviews: "interrupts synchronize state; tasks advance flow" (`中断只同步状态，任务才推进流程`).

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
