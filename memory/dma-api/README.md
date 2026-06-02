# DMA API

`dma-api` provides typed DMA ownership primitives for drivers. The public
surface separates three different concepts that should not be mixed:

- `CoherentArray<T>` / `CoherentBox<T>`: CPU and device see the same memory
  without explicit cache maintenance. Use these for descriptor rings, command
  queues, completion queues, and controller contexts. Coherency does not
  provide ordering; drivers still need their normal barriers before doorbells
  and after completion ownership changes.
- `ContiguousArray<T>` / `ContiguousBox<T>`: owned device-address-contiguous
  DMA memory using normal CPU mapping. Use these for data buffers and buffer
  pools. CPU-only accessors are named with a `_cpu` suffix; ownership transfer
  is explicit via `prepare_for_device(_all)` / `complete_for_cpu(_all)` or the
  higher-level `*_for_device` / `*_from_device` helpers.
- `StreamingMap<T>`: RAII mapping of an existing caller-owned buffer. Use this
  for one transfer of a buffer not owned by `dma-api`. Explicit sync methods
  handle cache maintenance and bounce-buffer copy, and `Drop` unmaps.

The `*_for_device` and `*_from_device` helpers are convenience ownership
transfer APIs. They wrap the same synchronization operations as
`prepare_for_device(_all)` and `complete_for_cpu(_all)`: CPU writes are made
visible before the device runs, and device writes are made visible before CPU
reads. They do not detect device completion, place MMIO doorbells, or provide
hardware ordering barriers. Drivers still decide when a transfer is submitted
and when it has completed.

`DmaAddr` is the only portable device-visible address type. Backend-private raw
handles are split into `DmaAllocHandle` for owned allocations and
`DmaMapHandle` for streaming mappings.

## Constraints

Every allocation and mapping is checked against `DmaConstraints`:

```rust,ignore
pub struct DmaConstraints {
    pub addr_mask: u64,
    pub align: usize,
    pub boundary: Option<usize>,
    pub max_segment_size: Option<usize>,
}
```

`DeviceDma::new(dma_mask, op)` is shorthand for
`DmaConstraints::new(dma_mask)`. Use `with_constraints` when a specific queue
or transfer has stronger alignment, boundary, or segment-size requirements.

Backends must never hand a driver a DMA address outside the requested mask. For
example, a device created with `DeviceDma::new(u32::MAX as u64, op)` must only
return 32-bit reachable DMA addresses. Streaming mappings may use a fast path
when the original buffer already satisfies the constraints; otherwise they
should allocate an in-mask bounce buffer.

## Backend Contract

Implement `DmaOp` once for the platform:

```rust,ignore
use core::{alloc::Layout, num::NonZeroUsize, ptr::NonNull};
use dma_api::{
    DmaAllocHandle, DmaConstraints, DmaDirection, DmaError, DmaMapHandle, DmaOp,
};

struct MyDma;

impl DmaOp for MyDma {
    fn page_size(&self) -> usize {
        4096
    }

    unsafe fn alloc_contiguous(
        &self,
        constraints: DmaConstraints,
        layout: Layout,
    ) -> Option<DmaAllocHandle> {
        todo!("allocate normal mapped, device-visible contiguous DMA memory")
    }

    unsafe fn dealloc_contiguous(&self, handle: DmaAllocHandle) {
        todo!("free alloc_contiguous memory")
    }

    unsafe fn alloc_coherent(
        &self,
        constraints: DmaConstraints,
        layout: Layout,
    ) -> Option<DmaAllocHandle> {
        todo!("allocate the same constrained memory and apply coherent policy")
    }

    unsafe fn dealloc_coherent(&self, handle: DmaAllocHandle) {
        todo!("restore mapping policy and free alloc_coherent memory")
    }

    unsafe fn map_streaming(
        &self,
        constraints: DmaConstraints,
        addr: NonNull<u8>,
        size: NonZeroUsize,
        direction: DmaDirection,
    ) -> Result<DmaMapHandle, DmaError> {
        todo!("map the existing buffer or create an in-constraint bounce buffer")
    }

    unsafe fn unmap_streaming(&self, handle: DmaMapHandle) {
        todo!("unmap and release any bounce allocation")
    }
}
```

The default sync methods perform cache maintenance and handle bounce-buffer
copying. Platforms can override them if the architecture needs a different
policy.

## Driver Usage

Descriptor/control memory:

```rust,ignore
let mut ring = dma.coherent_array_zero_with_align::<Descriptor>(256, 64)?;
ring.set_cpu(0, Descriptor::new(buffer_dma));
doorbell_after_release_barrier();
```

Owned data buffers:

```rust,ignore
let mut tx = dma.contiguous_array_zero_with_align::<u8>(
    len,
    64,
    DmaDirection::ToDevice,
)?;
tx.copy_to_device_from_slice(packet);
submit(tx.dma_addr());
```

Device-written owned buffers:

```rust,ignore
let rx = dma.contiguous_array_zero_with_align::<u8>(
    len,
    64,
    DmaDirection::FromDevice,
)?;
submit(rx.dma_addr());
wait_complete();
rx.read_from_device(packet_len, consume);
```

Streaming mappings:

```rust,ignore
let map = dma.map_streaming_slice_for_device(buffer, 64, DmaDirection::Bidirectional)?;
submit(map.dma_addr());
wait_complete();
map.complete_for_cpu_all();
```

Buffer pools use `ContiguousBufferPool` and return `ContiguousBuffer` values.
They are intended for repeated owned data buffers such as network RX/TX pools
or block read buffers. Reusing a buffer does not imply that the memory is
zeroed again; callers own the content and the explicit sync points.

## Choosing A Primitive

Use `Coherent*` for hardware metadata whose CPU and device ownership flips
frequently and where per-transfer cache maintenance would be wrong or too
fragile: xHCI contexts and rings, NVMe SQ/CQ, network descriptor rings,
SD/MMC descriptor tables.

Use `Contiguous*` for owned payload memory that needs a contiguous
device-visible DMA address range but not an uncached CPU mapping: block data,
network pools, NVMe PRP data buffers, and accelerator input/output buffers.

Use `StreamingMap` for caller-owned buffers whose lifetime is tied to one DMA
operation: USB transfer buffers, SDHCI/DWMMC/Phytium MCI block request slices,
or any buffer allocated outside the DMA API.
