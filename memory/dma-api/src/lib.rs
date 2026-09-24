#![cfg_attr(target_os = "none", no_std)]
#![doc = include_str!("../README.md")]

extern crate alloc;

use alloc::sync::Arc;
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
    backend: DmaBackend,
    info: DmaDeviceInfo,
}

#[derive(Clone)]
enum DmaBackend {
    Static(&'static dyn DmaOp),
    Shared(Arc<dyn DmaOp>),
}

impl DeviceDma {
    /// Creates a direct DMA capability backed by a static kernel allocator.
    ///
    /// Translated devices must use `new_shared` so the bound domain remains
    /// alive for all cloned capabilities and outstanding DMA handles.
    pub fn new(info: DmaDeviceInfo, op: &'static dyn DmaOp) -> Self {
        assert!(matches!(info.domain(), DmaDomainId::Direct));
        assert!(matches!(op.domain_id(), DmaDomainId::Direct));
        Self {
            info,
            backend: DmaBackend::Static(op),
        }
    }

    /// Creates a device capability owning its backend and IOMMU domain.
    pub fn new_shared(info: DmaDeviceInfo, op: Arc<dyn DmaOp>) -> Result<Self, DmaError> {
        let backend = op.domain_id();
        let requested = info.domain();
        if requested != backend {
            return Err(DmaError::DomainMismatch { requested, backend });
        }
        Ok(Self {
            info,
            backend: DmaBackend::Shared(op),
        })
    }

    fn op(&self) -> &dyn DmaOp {
        match &self.backend {
            DmaBackend::Static(op) => *op,
            DmaBackend::Shared(op) => op.as_ref(),
        }
    }

    pub fn with_constraints(&self, constraints: DmaConstraints) -> Self {
        Self {
            backend: self.backend.clone(),
            info: self.info.with_constraints(constraints),
        }
    }

    pub const fn info(&self) -> DmaDeviceInfo {
        self.info
    }

    pub fn page_size(&self) -> usize {
        self.op().page_size()
    }

    pub(crate) unsafe fn alloc_contiguous(
        &self,
        layout: core::alloc::Layout,
    ) -> Result<DmaAllocHandle, DmaError> {
        let mut constraints = self.info.constraints();
        constraints.align = constraints.align.max(layout.align());
        let res = unsafe { self.op().try_alloc_contiguous(constraints, layout) }?;
        match self.check_alloc_handle(&res, constraints) {
            Ok(()) => Ok(res),
            Err(e) => {
                if let Err(release_err) = unsafe { self.op().try_dealloc_contiguous(res) } {
                    log::error!(
                        "failed to release invalid DMA allocation; allocation quarantined: \
                         {release_err}"
                    );
                }
                Err(e)
            }
        }
    }

    pub(crate) unsafe fn dealloc_contiguous(&self, handle: DmaAllocHandle) -> Result<(), DmaError> {
        unsafe { self.op().try_dealloc_contiguous(handle) }
    }

    pub(crate) unsafe fn alloc_coherent(
        &self,
        layout: core::alloc::Layout,
    ) -> Result<DmaAllocHandle, DmaError> {
        let mut constraints = self.info.constraints();
        constraints.align = constraints.align.max(layout.align());
        let res = match self.info.coherency() {
            DmaCoherency::Coherent => unsafe {
                self.op().try_alloc_contiguous(constraints, layout)
            },
            DmaCoherency::NonCoherent => unsafe {
                self.op().try_alloc_coherent(constraints, layout)
            },
        }?;
        match self.check_alloc_handle(&res, constraints) {
            Ok(()) => Ok(res),
            Err(e) => {
                match self.info.coherency() {
                    DmaCoherency::Coherent => {
                        if let Err(release_err) = unsafe { self.op().try_dealloc_contiguous(res) } {
                            log::error!(
                                "failed to release invalid DMA allocation; allocation \
                                 quarantined: {release_err}"
                            );
                        }
                    }
                    DmaCoherency::NonCoherent => {
                        if let Err(release_err) = unsafe { self.op().dealloc_coherent(res) } {
                            log::error!(
                                "failed to release invalid coherent DMA allocation; allocation \
                                 quarantined: {release_err}"
                            );
                        }
                    }
                }
                Err(e)
            }
        }
    }

    pub(crate) unsafe fn dealloc_coherent(&self, handle: DmaAllocHandle) -> Result<(), DmaError> {
        match self.info.coherency() {
            DmaCoherency::Coherent => unsafe { self.op().try_dealloc_contiguous(handle) },
            DmaCoherency::NonCoherent => unsafe { self.op().dealloc_coherent(handle) },
        }
    }

    pub(crate) unsafe fn map_streaming(
        &self,
        addr: NonNull<u8>,
        size: NonZeroUsize,
        align: usize,
        direction: DmaDirection,
    ) -> Result<DmaMapHandle, DmaError> {
        let mut constraints = self.info.constraints();
        constraints.align = constraints.align.max(align);
        let res = unsafe { self.op().map_streaming(constraints, addr, size, direction) }?;
        match self.check_map_handle(&res, constraints) {
            Ok(()) => Ok(res),
            Err(e) => {
                if let Err(release_err) = unsafe { self.op().try_unmap_streaming(res) } {
                    log::error!(
                        "failed to release invalid DMA mapping; mapping quarantined: {release_err}"
                    );
                }
                Err(e)
            }
        }
    }

    pub(crate) unsafe fn unmap_streaming(&self, handle: DmaMapHandle) -> Result<(), DmaError> {
        unsafe { self.op().try_unmap_streaming(handle) }
    }

    pub(crate) fn sync_alloc_for_device(
        &self,
        handle: &DmaAllocHandle,
        offset: usize,
        size: usize,
        direction: DmaDirection,
    ) {
        if self.info.coherency() == DmaCoherency::NonCoherent {
            self.op()
                .sync_alloc_for_device(handle, offset, size, direction);
        }
    }

    pub(crate) fn sync_alloc_for_cpu(
        &self,
        handle: &DmaAllocHandle,
        offset: usize,
        size: usize,
        direction: DmaDirection,
    ) {
        if self.info.coherency() == DmaCoherency::NonCoherent {
            self.op()
                .sync_alloc_for_cpu(handle, offset, size, direction);
        }
    }

    pub(crate) fn sync_map_for_device(
        &self,
        handle: &DmaMapHandle,
        offset: usize,
        size: usize,
        direction: DmaDirection,
    ) {
        self.op()
            .sync_map_for_device(handle, offset, size, direction, self.info.coherency());
    }

    pub(crate) fn sync_map_for_cpu(
        &self,
        handle: &DmaMapHandle,
        offset: usize,
        size: usize,
        direction: DmaDirection,
    ) {
        self.op()
            .sync_map_for_cpu(handle, offset, size, direction, self.info.coherency());
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

    pub fn map_streaming_slice<'a, T: DmaPod>(
        &self,
        buff: &'a mut [T],
        align: usize,
        direction: DmaDirection,
    ) -> Result<StreamingMap<'a, T>, DmaError> {
        StreamingMap::map(self, buff, align, direction)
    }

    pub fn map_streaming_slice_for_device<'a, T: DmaPod>(
        &self,
        buff: &'a mut [T],
        align: usize,
        direction: DmaDirection,
    ) -> Result<StreamingMap<'a, T>, DmaError> {
        let map = self.map_streaming_slice(buff, align, direction)?;
        map.prepare_for_device(0..map.bytes_len());
        Ok(map)
    }

    /// Maps a caller-owned buffer whose lifetime is managed outside Rust's
    /// borrow checker, such as an asynchronous request stored by a bus driver.
    ///
    /// # Safety
    ///
    /// `ptr` must be aligned and valid for `len` initialized `T` values,
    /// with exclusive CPU ownership for the mapping lifetime. The allocation
    /// must remain live and at a stable address until the returned map is
    /// dropped, including cancellation and error paths. The device must stop
    /// accessing the buffer before that drop. A translated backend must not
    /// retain access to these borrowed physical pages after unmap failure;
    /// use backend-owned bounce pages when invalidation can fail.
    pub unsafe fn map_streaming_raw<T: DmaPod>(
        &self,
        ptr: NonNull<T>,
        len: usize,
        align: usize,
        direction: DmaDirection,
    ) -> Result<StreamingMap<'static, T>, DmaError> {
        unsafe { StreamingMap::map_raw(self, ptr, len, align, direction) }
    }

    #[cfg(feature = "pool")]
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
