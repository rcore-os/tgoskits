---
name: create-driver
description: "Use when creating, refactoring, or maintaining a bare-metal driver in this repository; covers kernel-provided dma-api and mmio-api integration, driver construction patterns, bare-test usage, and validation commands."
---

# Create Driver

Use this skill when creating a new bare-metal driver, refactoring an existing one to use repository abstractions, or restoring a broken driver and its tests in this repository.

## Goals

- Create drivers that follow Sparreal OS layering instead of embedding platform logic in the driver.
- Reuse `dma-api` and `mmio-api` through kernel-provided ops.
- Keep the driver constructor explicit about required capabilities.
- Validate the driver through `bare-test` and repository-standard no_std test commands.

## Workflow

### 1. Confirm the driver boundary

- Put hardware-independent driver logic in the driver crate.
- Put MMIO and DMA platform mechanics in `sparreal-kernel::os::mem`.
- Avoid making the driver know about `sparreal-rt`, `somehal`, or platform-specific mapping details.
- Treat the constructor as the capability boundary: pass in addresses, masks, config, and required ops explicitly.

### 2. Start from repository abstractions

- Use `mmio-api` for BAR or register-space mapping.
- Use `dma-api` for coherent allocation and DMA buffer management.
- Prefer kernel-provided implementations such as:
  - `sparreal_kernel::os::mem::mmio::kernel_mmio_op()`
  - `sparreal_kernel::os::mem::dma::kernel_dma_op()`
- If the driver needs higher-level block integration, add an adapter layer instead of mixing framework logic into the low-level transport.

### 3. Design the constructor around explicit ops

Use a constructor shape that makes dependencies obvious.

- Accept MMIO physical address and size, not raw mapped pointers, as the stable public entry point.
- Accept a DMA mask if device addressing limits matter.
- Accept `&'static dyn mmio_api::MmioOp` and `&'static dyn dma_api::DmaOp` when the driver should remain runtime-agnostic.
- Initialize `mmio-api` from the provided `MmioOp` before mapping.
- Build `dma_api::DeviceDma` from the provided `DmaOp` rather than constructing custom per-test DMA stubs unless the test truly needs one.

### 4. Keep kernel memory services in `os::mem`

- Put MMIO helpers in `sparreal-kernel::os::mem::mmio`.
- Put DMA helpers in `sparreal-kernel::os::mem::dma`.
- Reuse `kernel_memory_allocator()` for DMA-capable coherent memory when possible.
- Keep address conversion through `VirtAddr -> PhysAddr` and existing kernel memory helpers.
- Add mask-aware allocation helpers in the kernel if a device requires low-address DMA.

### 5. Keep runtime layers thin

- `sparreal-rt` should mainly re-export or delegate kernel memory ops.
- `bare-test` should reuse the same exported ops instead of defining a separate DMA/MMIO model.
- If a driver test can use kernel-provided ops directly, prefer that over duplicating trait implementations in the test.

### 6. Create the bare-test first, then restore driver logic in phases

1. Create the minimal `bare-test` skeleton.
2. Confirm boot and timeout handling.
3. Parse platform descriptor and FDT.
4. Set up bus discovery, such as PCIe host bridge ranges.
5. Discover the target endpoint.
6. Construct the driver with kernel `dma/mmio` ops.
7. Verify admin or control-path operations.
8. Verify data-path operations.
9. If needed, add an adapter such as `rd-block` only after the low-level path is stable.

### 7. Validate every layer with explicit markers

- Print a marker before each major phase.
- Put `#[timeout = ...]` on hardware-touching tests.
- When a phase fails, stop and isolate that phase rather than restoring the full test body.
- Use one end-to-end hardware test with internal phase markers when repeated hardware setup across multiple tests becomes unstable.

## dma-api Usage

- Use `dma_api::DeviceDma::new(dma_mask, dma_op)` inside the driver.
- Use coherent arrays or boxes for command structures, queues, and data buffers.
- Respect device DMA mask constraints; if the device is 32-bit limited, allocate from low-address memory.
- Let `DmaOp` own mapping, unmapping, and coherent allocation behavior.
- Avoid ad hoc raw-pointer DMA management in drivers when `DeviceDma` already expresses the lifecycle.

## mmio-api Usage

- Use `mmio_api::init(mmio_op)` before calling `mmio_api::ioremap(...)` if the process has not already set the op.
- Map from physical address and size, then retain the `Mmio` object so drop-based unmap remains available.
- Convert the mapped virtual pointer into typed register access only after the mapping succeeds.
- Keep the `Mmio` field in the driver if the mapping lifetime must match the driver lifetime.
- Avoid public APIs that require callers to pre-map registers unless there is a strong reason.

## bare-test Pattern

- Prefer `#[bare_test::tests]` with `#![no_std]` and `#![no_main]`.
- Use `cargo t`, not plain `cargo test`, for bare-metal test flows.
- Put runtime QEMU arguments in the package-local `.qemu.toml`.
- Keep success and failure regex configured so the runner can terminate QEMU deterministically.
- In tests, prefer kernel-exported ops over custom local implementations unless the test is specifically for DMA or MMIO behavior itself.

## Decision Points

- If the driver constructor needs mapped pointers from callers, first ask whether that mapping can move inside the driver.
- If tests duplicate DMA/MMIO trait implementations, first ask whether kernel ops can be passed directly.
- If device initialization hangs, isolate the constructor and admin path before debugging the data path.
- If repeated multi-test hardware setup becomes flaky, merge hardware stages into one end-to-end test with clear markers.
- If the driver needs framework integration such as `rd-block`, add it after the raw device path is already stable.

## Completion Checks

- The driver constructor accepts the minimum explicit hardware inputs and trait dependencies it truly needs.
- MMIO mapping is performed through `mmio-api`, not manual pointer conversion alone.
- DMA allocation and mapping are performed through `dma-api` abstractions.
- Kernel owns the concrete DMA/MMIO implementations under `sparreal-kernel::os::mem`.
- `sparreal-rt` and `bare-test` can pass through the same ops without redefining platform behavior.
- `cargo check` passes for the driver and relevant runtime crates.
- `cargo t -p <driver> --target aarch64-unknown-none-softfloat` passes.

## Testing Commands

- Compile the driver and related runtime crates:

```bash
cargo check -p sparreal-kernel -p sparreal-rt -p bare-test -p <driver> --target aarch64-unknown-none-softfloat
```

- Run the driver bare-test through the repository runner:

```bash
cargo t -p <driver> --target aarch64-unknown-none-softfloat
```

- Show serial output while restoring a failing test:

```bash
cargo t -p <driver> --target aarch64-unknown-none-softfloat -- --show-output
```

- Run the repository script if one exists for that driver:

```bash
./scripts/test_<driver>.sh
```

- Format after refactors:

```bash
cargo fmt --all
```

- Run clippy for AArch64 target when the driver is stable enough:

```bash
cargo clippy --target aarch64-unknown-none-softfloat -- -D warnings
```

## Anti-Patterns

- Do not embed platform-specific `ioremap` or DMA allocator logic directly in the driver.
- Do not require tests to redefine DMA/MMIO ops if kernel exports suitable implementations.
- Do not expose raw mapped register pointers as the preferred public constructor API without a strong reason.
- Do not restore the full integration stack in one edit when debugging a failing bare-metal driver.
- Do not rely on plain `cargo test` to validate a `bare-test` workflow.

## Example Prompts

- Create a new PCIe bare-metal driver in this repo using kernel `dma-api` and `mmio-api` integrations.
- Refactor this driver so `Nvme::new` accepts hardware addresses plus injected `DmaOp` and `MmioOp` traits.
- Replace custom DMA test stubs with `sparreal-kernel::os::mem::dma::kernel_dma_op()` in this bare-test.
- Stage a new driver test from boot smoke test to full device I/O and keep each phase timeout-protected.