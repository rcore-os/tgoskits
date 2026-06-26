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
        let dma = DeviceDma::new_legacy(u32::MAX as u64, axklib::dma::op());
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

    /// Allocation length in bytes (page-rounded). Crate-internal: the type lives in a
    /// `pub(crate)` module, so this is its true visibility and keeps clippy's
    /// `len_without_is_empty` from demanding an `is_empty` no caller needs.
    pub(crate) fn len(&self) -> usize {
        self.inner.bytes_len()
    }

    /// Physical range for mmap.
    pub fn phys_range(&self) -> PhysAddrRange {
        // `as usize` is lossless: the 32-bit DMA mask bounds phys_addr to <= u32::MAX, and all
        // supported kernel targets are 64-bit (usize == u64).
        PhysAddrRange::from_start_size(PhysAddr::from(self.phys_addr() as usize), self.len())
    }

    /// CPU view of the buffer (used by the RGA selftest to verify hardware output).
    #[cfg(feature = "rga-selftest")]
    pub fn cpu_bytes(&self) -> &[u8] {
        self.inner.as_slice_cpu()
    }

    /// Mutable CPU view, for pre-seeding the buffer before a device write (e.g. the RGA selftest
    /// poisons the destination with a sentinel so it can tell "engine wrote nothing" from "wrote
    /// zeros"). A real dma-buf is CPU-writable via mmap; this is the in-kernel equivalent. Requires
    /// `&mut self` (use `Arc::get_mut` on a freshly-allocated, not-yet-shared buffer).
    ///
    /// # Safety
    /// Caller must not retain the slice across a device submission, and must
    /// `sync_for_device()` afterwards so the device sees the writes (the backing is CACHED).
    #[cfg(feature = "rga-selftest")]
    pub unsafe fn cpu_bytes_mut(&mut self) -> &mut [u8] {
        unsafe { self.inner.as_mut_slice_cpu() }
    }

    /// Hand ownership to the device before it accesses the buffer: cleans (flushes) dirty CPU cache
    /// lines to DRAM. The contiguous DMA backing is CACHED on aarch64 (NOT uncached — only the
    /// `alloc_coherent` path is uncached), and the allocation is zero-initialised via the CPU, so
    /// those zero lines are dirty; without this clean a later eviction can clobber the device's
    /// output. Bidirectional direction → this performs the clean.
    pub fn sync_for_device(&self) {
        self.inner.prepare_for_device_all();
    }

    /// Reclaim ownership for the CPU after a device write: invalidates the CPU cache so reads see
    /// the device's DRAM writes rather than stale cached data. Bidirectional → this invalidates.
    pub fn sync_for_cpu(&self) {
        self.inner.complete_for_cpu_all();
    }
}
