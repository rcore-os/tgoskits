# Cross-Kernel Driver Architecture Reference

Read this when creating, optimizing, or reviewing drivers under `drivers/`.

## Source Principles

The core problem is OS API coupling, not Rust portability. Hardware logic is usually stable; lock types, task models, DMA allocation/sync, MMIO remap, and IRQ registration vary by kernel.

Keep this mapping:

- Driver Core: registers, register access order, state machine, descriptor format, queue logic, request completion, event extraction.
- Capability Boundary: OS Trait / Driver Trait seam for MMIO, DMA, IRQ event, queue contract, wake boundary.
- OS Glue: probe, remap/iomap, IRQ registration, FDT/ACPI/PCI discovery, thread or worker spawn, OS wakeup APIs.
- Runtime: blocking / poll / future / worker wrappers.

Do not pursue one big `KernelHal` in production code. Split by lifetime and semantics: MMIO, DMA, IRQ, task progression, and queues usually have different owners and hot paths.

## Project Layout

Reusable hardware/IP crates live under `drivers/` by device type:

```text
drivers/<device-type>/<crate-name>
drivers/<device-type>/<vendor-or-family>/<crate-name>
```

Use descriptive device-type directories for reusable crates. For example, a new reusable block device may live under `drivers/block/<vendor>/<crate>`. Existing platform glue may use shorter historical module names such as `platform/axplat-dyn/src/drivers/blk`; keep those names when editing that layer.

Existing examples:

- `drivers/npu/rockchip-npu`
- `drivers/soc/rockchip/rockchip-pm`
- `drivers/soc/rockchip/rockchip-soc`

When adding a new reusable crate:

1. Add it to root workspace `members`.
2. Add it to root `[workspace.dependencies]` if other workspace crates consume it.
3. Keep crate `src/` portable and `#![no_std]` friendly when practical.
4. Keep OS-specific crates out of normal `[dependencies]`.
5. Prefer `<dep>.workspace = true` in member `Cargo.toml` files when the dependency is already present in root `[workspace.dependencies]`; avoid direct version duplication for new code.

Runtime/platform integration belongs elsewhere:

- `platform/axplat-dyn/src/drivers/<type>/...` for dynamic platform/FDT/probe glue.
- `platform/axplat-dyn/src/drivers/soc/<vendor>/...` for SoC platform glue.
- `components/axdriver_crates/axdriver_<type>` for common ArceOS-facing driver traits/adapters.

For block devices in `axplat-dyn`, the existing integration path is:

- probe/FDT/MMIO setup in `platform/axplat-dyn/src/drivers/blk/<driver>.rs`
- wrap the portable driver in an `rd_block::Interface`
- expose a queue as `rd_block::IQueue`
- register through `PlatformDeviceBlock::register_block`, which creates `rd_block::Block::new(dev, &DmaImpl)` and registers it with `rdrive`

Keep `rd-block`/`rdrive` coupling in this adapter layer or behind an explicit adapter feature. Portable driver core should not need to know how `rdrive` probes or registers devices.

## Dependency Boundaries

Use `dma-api` and `mmio-api` as portable capability APIs.

Current versions observed on 2026-04-28:

```toml
mmio-api = "0.2.1"
dma-api = "0.7.2"
```

Before changing versions, run:

```bash
cargo search mmio-api --limit 5
cargo search dma-api --limit 5
cargo info mmio-api
cargo info dma-api
```

`dma-api` is already present in this workspace root. `mmio-api` may need to be added to root `[workspace.dependencies]` if new code adopts it.

## MMIO Pattern

Portable core should receive an already mapped register region or typed wrapper. Avoid direct calls to `axklib::mem::iomap`, `bare_test::mem::iomap`, Linux `ioremap`, or any target OS mapping API from reusable driver code.

Use `mmio-api` in glue:

- Implement `mmio_api::MmioOp` for the target OS/platform.
- Call `mmio_api::ioremap` or equivalent glue-side mapping during probe/setup.
- Pass `mmio_api::Mmio`, `mmio_api::MmioRaw`, `NonNull<u8>`, or a typed register wrapper into Driver Core.
- Keep `mmio_api::MmioRaw::new`, raw pointer casts, and map lifetime assumptions inside the boundary.

Existing `axplat-dyn` glue has local helpers such as `crate::drivers::iomap` that return `NonNull<u8>`. For new code, prefer wrapping the OS mapping operation in a `mmio_api::MmioOp` implementation. When adapting an existing driver that still takes `NonNull<u8>`, it is acceptable to map with `mmio-api` in glue and pass `MmioRaw::as_nonnull_ptr()` while keeping the owning `Mmio`/mapping lifetime in the adapter.

Driver core may use volatile register access through `mmio-api`, `tock-registers`, or a small typed wrapper matching the existing crate style.

## DMA Pattern

OS Glue implements:

```rust
impl dma_api::DmaOp for DmaImpl { /* platform allocator and cache ops */ }
```

Device setup creates:

```rust
let dma = dma_api::DeviceDma::new(dma_mask, &DMA_IMPL);
```

Driver Core should use `DeviceDma` and `dma-api` abstractions:

- `DArray` / `DBox` for coherent descriptor rings, command buffers, and fixed DMA-owned data.
- `map_single_array` / `SArrayPtr` for mapping existing buffers.
- `DmaDirection::{ToDevice, FromDevice, Bidirectional}` to make cache sync semantics explicit.
- `DmaAddr` for device-visible bus addresses.

Check every DMA path for:

- mask/address width
- alignment
- page/layout size
- cache flush/invalidate direction
- map/unmap or alloc/dealloc pairing
- ownership transfer and zero-copy lifetime
- distinction between CPU virtual address and bus/DMA address

## IRQ/Event Pattern

Portable IRQ handling should answer "what happened?" OS Glue answers "how should execution continue?"

Use an IRQ handle that extracts a stable event:

```rust
pub trait IrqHandle {
    fn handle_irq(&self) -> Event;
}
```

`Event` should identify:

- event kind
- affected queue or engine
- completion state
- error or recovery state

The IRQ fast path should:

- identify the interrupt source
- read and clear required status registers
- return a stable event object
- avoid blocking, long work, and broad locks

OS Glue converts events into wakeups, future wakers, worker scheduling, or pending polling flags.

## Queue/Runtime Pattern

Model queues as independent running units. This matches network TX/RX queues, NVMe admin/IO queues, block request queues, and many accelerator command queues.

Common actions:

- `submit`
- `reclaim`
- `poll`
- `submit_request`
- `poll_request`

Runtime wrappers can then choose:

- blocking loop over poll
- IRQ-driven wakeup
- `Future::poll`
- worker thread/task per queue

Avoid a single global `Driver::poll` if the hardware naturally exposes multiple queues or engines. Avoid a "big object + big lock + callbacks" shape unless the device is truly that simple.

For a block queue adapter, align portable queue state with `rd_block::IQueue`:

- `buff_config()` should expose block-size, alignment, and DMA mask constraints.
- `submit_request()` should allocate/map DMA buffers, program descriptors, and return a request id.
- `poll_request()` should check completion and translate device status into `rd_block::BlkError` in the adapter.
- Keep descriptor ownership and DMA map/unmap pairing explicit for each request id.

## Concurrency Rules

- Prefer `&mut self` for externally visible operations that require exclusive access.
- Do not make OS locks part of the portable Driver Trait.
- Use internal locks only for short critical sections such as pending flags or small status updates.
- Keep `unsafe` in callback bridges, MMIO construction, and DMA glue boundaries where possible.
- Do slow work in task/worker/executor/polling context, not in IRQ context.

## Review Checklist

- Does `src/` stay OS independent?
- Are OS crates limited to `dev-dependencies`, platform glue, or explicit adapter crates?
- Is MMIO mapping handled by `mmio-api` or a clear OS Glue boundary?
- Is DMA handled through `dma-api` with mask, alignment, direction, lifetime, and address-type clarity?
- Does IRQ code return events rather than directly performing OS notification?
- Are queues independent enough to support blocking / poll / future / worker runtimes?
- Did validation include `cargo fmt` and targeted `cargo xtask clippy --package <crate>`?
