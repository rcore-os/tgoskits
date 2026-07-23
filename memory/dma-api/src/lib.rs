#![cfg_attr(target_os = "none", no_std)]
#![doc = include_str!("../README.md")]

extern crate alloc;

use core::{num::NonZeroUsize, ptr::NonNull};

mod op;

mod array;
mod common;
mod dbox;
mod def;
mod owned;
mod pool;
mod streaming;

pub use array::*;
pub use dbox::*;
pub use def::*;
pub use op::DmaOp;
pub use owned::*;
pub use pool::*;
pub use streaming::*;

#[derive(Clone)]
pub struct DeviceDma {
    op: &'static dyn DmaOp,
    constraints: DmaConstraints,
    domain: DmaDomainId,
}

impl DeviceDma {
    pub fn new(domain: DmaDomainId, dma_mask: u64, op: &'static dyn DmaOp) -> Self {
        Self {
            constraints: DmaConstraints::new(dma_mask),
            domain,
            op,
        }
    }

    pub fn new_identity(dma_mask: u64, op: &'static dyn DmaOp) -> Self {
        Self::new(DmaDomainId::identity(), dma_mask, op)
    }

    pub fn with_constraints(&self, constraints: DmaConstraints) -> Self {
        Self {
            op: self.op,
            constraints,
            domain: self.domain,
        }
    }

    pub fn constraints(&self) -> DmaConstraints {
        self.constraints
    }

    pub fn dma_mask(&self) -> u64 {
        self.constraints.addr_mask
    }

    pub fn domain_id(&self) -> DmaDomainId {
        self.domain
    }

    /// Verifies that an imported DMA resource belongs to this device domain.
    pub fn validate_domain(&self, actual: DmaDomainId) -> Result<(), DmaError> {
        if actual == self.domain {
            Ok(())
        } else {
            Err(DmaError::DomainMismatch {
                expected: self.domain,
                actual,
            })
        }
    }

    pub fn flush(&self, addr: NonNull<u8>, size: usize) {
        self.op.flush(addr, size)
    }

    pub fn invalidate(&self, addr: NonNull<u8>, size: usize) {
        self.op.invalidate(addr, size)
    }

    pub fn flush_invalidate(&self, addr: NonNull<u8>, size: usize) {
        self.op.flush_invalidate(addr, size)
    }

    pub fn page_size(&self) -> usize {
        self.op.page_size()
    }

    pub(crate) unsafe fn alloc_contiguous(
        &self,
        layout: core::alloc::Layout,
    ) -> Result<DmaAllocHandle, DmaError> {
        if layout.size() == 0 {
            return Err(DmaError::ZeroSizedBuffer);
        }
        let constraints = self.constraints.with_align(layout.align());
        let res =
            unsafe { self.op.alloc_contiguous(constraints, layout) }.ok_or(DmaError::NoMemory)?;
        match self.check_alloc_handle(&res, constraints) {
            Ok(()) => Ok(res),
            Err(e) => {
                unsafe { self.op.dealloc_contiguous(res) };
                Err(e)
            }
        }
    }

    pub(crate) unsafe fn dealloc_contiguous(&self, handle: DmaAllocHandle) {
        unsafe { self.op.dealloc_contiguous(handle) }
    }

    /// Allocates a coherent buffer and returns its move-only backend token.
    ///
    /// Prefer the owned coherent container APIs. This low-level entry exists
    /// for external driver traits that split allocation and deallocation.
    ///
    /// # Safety
    ///
    /// The returned token must be consumed exactly once by
    /// [`Self::dealloc_coherent`].
    pub unsafe fn alloc_coherent(
        &self,
        layout: core::alloc::Layout,
    ) -> Result<DmaAllocHandle, DmaError> {
        if layout.size() == 0 {
            return Err(DmaError::ZeroSizedBuffer);
        }
        let constraints = self.constraints.with_align(layout.align());
        let res =
            unsafe { self.op.alloc_coherent(constraints, layout) }.ok_or(DmaError::NoMemory)?;
        match self.check_alloc_handle(&res, constraints) {
            Ok(()) => Ok(res),
            Err(e) => {
                unsafe { self.op.dealloc_coherent(res) };
                Err(e)
            }
        }
    }

    /// Releases a coherent allocation token.
    ///
    /// # Safety
    ///
    /// `handle` must have been returned by [`Self::alloc_coherent`] for this
    /// device and must not have been consumed previously.
    pub unsafe fn dealloc_coherent(&self, handle: DmaAllocHandle) {
        unsafe { self.op.dealloc_coherent(handle) }
    }

    /// Creates a streaming mapping and returns its move-only backend token.
    ///
    /// Prefer [`Self::map_streaming_slice`]. This entry supports external
    /// driver traits whose ABI separates share and unshare calls.
    ///
    /// # Safety
    ///
    /// The buffer must remain live and obey the DMA ownership rules until the
    /// token is consumed by [`Self::unmap_streaming`].
    pub unsafe fn map_streaming(
        &self,
        addr: NonNull<u8>,
        size: NonZeroUsize,
        align: usize,
        direction: DmaDirection,
    ) -> Result<DmaMapHandle, DmaError> {
        let constraints = self.constraints.with_align(align);
        let res = unsafe { self.op.map_streaming(constraints, addr, size, direction) }?;
        match self.check_map_handle(&res, constraints) {
            Ok(()) => Ok(res),
            Err(e) => {
                unsafe { self.op.unmap_streaming(res) };
                Err(e)
            }
        }
    }

    /// Releases a streaming mapping token.
    ///
    /// # Safety
    ///
    /// `handle` must have been returned by [`Self::map_streaming`] for this
    /// device and must not have been consumed previously.
    pub unsafe fn unmap_streaming(&self, handle: DmaMapHandle) {
        unsafe { self.op.unmap_streaming(handle) }
    }

    pub(crate) fn sync_alloc_for_device(
        &self,
        handle: &DmaAllocHandle,
        offset: usize,
        size: usize,
        direction: DmaDirection,
    ) {
        self.op
            .sync_alloc_for_device(handle, offset, size, direction);
    }

    pub(crate) fn sync_alloc_for_cpu(
        &self,
        handle: &DmaAllocHandle,
        offset: usize,
        size: usize,
        direction: DmaDirection,
    ) {
        self.op.sync_alloc_for_cpu(handle, offset, size, direction);
    }

    /// Transfers a mapped range from CPU ownership to device ownership.
    pub fn sync_map_for_device(
        &self,
        handle: &DmaMapHandle,
        offset: usize,
        size: usize,
        direction: DmaDirection,
    ) {
        self.op.sync_map_for_device(handle, offset, size, direction);
    }

    /// Transfers a mapped range from device ownership to CPU ownership.
    pub fn sync_map_for_cpu(
        &self,
        handle: &DmaMapHandle,
        offset: usize,
        size: usize,
        direction: DmaDirection,
    ) {
        self.op.sync_map_for_cpu(handle, offset, size, direction);
    }

    pub fn coherent_array_zero<T: DmaPod>(&self, len: usize) -> Result<CoherentArray<T>, DmaError> {
        CoherentArray::new_zero(self, len)
    }

    pub fn coherent_array_zero_with_align<T: DmaPod>(
        &self,
        len: usize,
        align: usize,
    ) -> Result<CoherentArray<T>, DmaError> {
        CoherentArray::new_zero_with_align(self, len, align)
    }

    pub fn contiguous_array_zero<T: DmaPod>(
        &self,
        len: usize,
        direction: DmaDirection,
    ) -> Result<ContiguousArray<T>, DmaError> {
        ContiguousArray::new_zero(self, len, direction)
    }

    pub fn contiguous_array_zero_with_align<T: DmaPod>(
        &self,
        len: usize,
        align: usize,
        direction: DmaDirection,
    ) -> Result<ContiguousArray<T>, DmaError> {
        ContiguousArray::new_zero_with_align(self, len, align, direction)
    }

    pub fn coherent_box_zero<T: DmaPod>(&self) -> Result<CoherentBox<T>, DmaError> {
        CoherentBox::new_zero(self)
    }

    pub fn coherent_box_zero_with_align<T: DmaPod>(
        &self,
        align: usize,
    ) -> Result<CoherentBox<T>, DmaError> {
        CoherentBox::new_zero_with_align(self, align)
    }

    pub fn contiguous_box_zero<T: DmaPod>(
        &self,
        direction: DmaDirection,
    ) -> Result<ContiguousBox<T>, DmaError> {
        ContiguousBox::new_zero(self, direction)
    }

    pub fn contiguous_box_zero_with_align<T: DmaPod>(
        &self,
        align: usize,
        direction: DmaDirection,
    ) -> Result<ContiguousBox<T>, DmaError> {
        ContiguousBox::new_zero_with_align(self, align, direction)
    }

    pub fn map_streaming_slice<T: DmaPod>(
        &self,
        buff: &mut [T],
        align: usize,
        direction: DmaDirection,
    ) -> Result<StreamingMap<T>, DmaError> {
        StreamingMap::map(self, buff, align, direction)
    }

    pub fn map_streaming_slice_for_device<T: DmaPod>(
        &self,
        buff: &mut [T],
        align: usize,
        direction: DmaDirection,
    ) -> Result<StreamingMap<T>, DmaError> {
        let map = self.map_streaming_slice(buff, align, direction)?;
        map.prepare_for_device_all();
        Ok(map)
    }

    pub fn contiguous_buffer_pool(
        &self,
        layout: core::alloc::Layout,
        direction: DmaDirection,
        cap: usize,
    ) -> ContiguousBufferPool {
        let config = ContiguousBufferConfig {
            size: layout.size(),
            align: layout.align(),
            direction,
        };
        ContiguousBufferPool::with_capacity(self.clone(), config, cap)
    }

    fn check_alloc_handle(
        &self,
        handle: &DmaAllocHandle,
        constraints: DmaConstraints,
    ) -> Result<(), DmaError> {
        check_dma_range(handle.dma_addr(), handle.size(), constraints)?;
        check_dma_align(handle.dma_addr(), handle.align().max(constraints.align))?;
        Ok(())
    }

    fn check_map_handle(
        &self,
        handle: &DmaMapHandle,
        constraints: DmaConstraints,
    ) -> Result<(), DmaError> {
        check_dma_range(handle.dma_addr(), handle.size(), constraints)?;
        check_dma_align(handle.dma_addr(), handle.align().max(constraints.align))?;
        Ok(())
    }
}

fn check_dma_range(
    addr: DmaAddr,
    size: usize,
    constraints: DmaConstraints,
) -> Result<(), DmaError> {
    let start = addr.as_u64();
    let in_mask = if size == 0 {
        start <= constraints.addr_mask
    } else {
        start
            .checked_add(size.saturating_sub(1) as u64)
            .map(|end| end <= constraints.addr_mask)
            .unwrap_or(false)
    };

    if !in_mask {
        return Err(DmaError::DmaMaskNotMatch {
            addr,
            mask: constraints.addr_mask,
        });
    }

    if let Some(max) = constraints.max_segment_size
        && size > max
    {
        return Err(DmaError::SegmentTooLarge { size, max });
    }

    if let Some(boundary) = constraints.boundary
        && size > 0
    {
        let boundary = boundary as u64;
        let end = start + size.saturating_sub(1) as u64;
        if start / boundary != end / boundary {
            return Err(DmaError::BoundaryCross {
                addr,
                size,
                boundary: boundary as usize,
            });
        }
    }

    Ok(())
}

fn check_dma_align(addr: DmaAddr, align: usize) -> Result<(), DmaError> {
    let align = align.max(1);
    if !addr.as_u64().is_multiple_of(align as u64) {
        return Err(DmaError::AlignMismatch {
            required: align,
            address: addr,
        });
    }
    Ok(())
}
