//! Kernel-owned dma-buf backing: one contiguous, coherent DMA allocation, shared via `Arc` across
//! the dma-buf fd, every mmap VMA, and any RGA import. Pages free when the last `Arc` drops.

use alloc::sync::Arc;

use ax_errno::{AxError, AxResult};
use ax_memory_addr::{PhysAddr, PhysAddrRange};
use dma_api::{ContiguousArray, DeviceDma, DmaDirection};

/// Page granularity. mmap requires a page-aligned physical base.
const DMA_BUF_ALIGN: usize = 4096;

/// A physically-contiguous, coherent DMA buffer exported as a dma-buf.
pub struct DmaBufObject {
    inner: ContiguousArray<u8>,
}

// SAFETY: `DmaBufObject` has no interior-mutable Rust state, so sharing `&DmaBufObject` across
// threads cannot cause a data race through the Rust type system: `cpu_bytes()` yields a read-only
// `&[u8]`, and no `&self` method mutates Rust-visible state. Coherency of the underlying physical
// memory between the CPU and the device is a hardware protocol governed externally by the dma-buf
// sync ABI (`sync_for_device`/`sync_for_cpu`), not a Rust aliasing concern. The manual impls are
// required because the wrapped `ContiguousArray<u8>` does not propagate `Send`/`Sync` automatically.
unsafe impl Send for DmaBufObject {}
unsafe impl Sync for DmaBufObject {}

impl DmaBufObject {
    /// Allocate `len` bytes (page-rounded) of contiguous, 32-bit-addressable coherent DMA memory.
    ///
    /// The 32-bit DMA mask matches RGA2, which programs a raw 32-bit physical base into its
    /// registers (see the RGA core `operation.rs` validation).
    pub fn alloc(len: usize) -> AxResult<Arc<Self>> {
        if len == 0 {
            return Err(AxError::InvalidInput);
        }
        let rounded = len
            .checked_next_multiple_of(DMA_BUF_ALIGN)
            .ok_or(AxError::InvalidInput)?;
        let dma = DeviceDma::new(u32::MAX as u64, axklib::dma::op());
        let inner = dma
            .contiguous_array_zero_with_align::<u8>(
                rounded,
                DMA_BUF_ALIGN,
                DmaDirection::Bidirectional,
            )
            .map_err(|_| AxError::NoMemory)?;
        Ok(Arc::new(Self { inner }))
    }

    /// Device (bus) physical base address.
    pub fn phys_addr(&self) -> u64 {
        self.inner.dma_addr().as_u64()
    }

    /// Allocation length in bytes (page-rounded).
    pub fn len(&self) -> usize {
        self.inner.bytes_len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Physical range for mmap.
    pub fn phys_range(&self) -> PhysAddrRange {
        // `as usize` is lossless: the 32-bit DMA mask bounds phys_addr to <= u32::MAX, and all
        // supported kernel targets are 64-bit (usize == u64).
        PhysAddrRange::from_start_size(PhysAddr::from(self.phys_addr() as usize), self.len())
    }

    /// CPU view of the buffer (used by the RGA selftest to verify hardware output).
    pub fn cpu_bytes(&self) -> &[u8] {
        self.inner.as_slice_cpu()
    }

    /// Hand ownership to a device (flush CPU writes). Under the aarch64 uncached contiguous
    /// allocator this is a no-op; retained so a future cached mode is a one-line change.
    pub fn sync_for_device(&self) {
        self.inner.prepare_for_device_all();
    }

    /// Reclaim ownership for the CPU (invalidate). No-op under the aarch64 uncached allocator.
    pub fn sync_for_cpu(&self) {
        self.inner.complete_for_cpu_all();
    }
}
