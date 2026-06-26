use alloc::{boxed::Box, vec::Vec};
use core::num::NonZeroUsize;

#[cfg(test)]
use dma_api::DmaOp;
use dma_api::{CompletedDma, ContiguousArray, CpuDmaBuffer, DeviceDma, DmaDirection};
use rdif_block::{BlkError, Segment, TransferChunk};

/// Guard retained in the pending table until the request reaches completion.
pub struct DmaBufferGuard {
    buffer: ContiguousArray<u8>,
    direction: DmaDirection,
    segments: Box<[Segment<'static>]>,
    len: usize,
}

// SAFETY: `DmaBufferGuard` owns the DMA buffer and every stored segment points
// into that buffer. Moving the guard between threads does not invalidate the
// backing allocation; the request completion contract still requires callers to
// keep the guard alive until `poll_request` reports completion.
unsafe impl Send for DmaBufferGuard {}

impl DmaBufferGuard {
    pub fn new(
        dma: &DeviceDma,
        len: usize,
        align: usize,
        direction: DmaDirection,
        chunk: TransferChunk,
        src: Option<&[u8]>,
    ) -> Result<Self, BlkError> {
        let mut buffer = dma
            .contiguous_array_zero_with_align(len.max(1), align.max(1), direction)
            .map_err(BlkError::from)?;
        let len = buffer.len();
        match direction {
            DmaDirection::FromDevice => buffer.prepare_for_device_all(),
            DmaDirection::ToDevice | DmaDirection::Bidirectional => {
                if let Some(src) = src {
                    buffer.copy_to_device_from_slice(src);
                } else {
                    buffer.prepare_for_device_all();
                }
            }
        }
        let base_virt = buffer.as_ptr().as_ptr();
        let base_bus = buffer.dma_addr();
        let planned_segments = chunk.segments();
        let mut segments = Vec::with_capacity(planned_segments.len());
        for segment in planned_segments {
            let virt = unsafe { base_virt.add(segment.byte_offset) };
            let bus = base_bus
                .checked_add(segment.byte_offset as u64)
                .ok_or(BlkError::InvalidRequest)?
                .as_u64();
            segments.push(unsafe { Segment::from_raw_parts(virt, bus, segment.byte_len) });
        }
        Ok(Self {
            buffer,
            direction,
            segments: segments.into_boxed_slice(),
            len,
        })
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns the request segments kept alive by this guard.
    ///
    /// # Safety
    ///
    /// The returned slice may be passed to `submit_request`. The caller must
    /// keep this guard alive and must not access the segments again until the
    /// matching request has completed.
    pub unsafe fn segments_for_submit(&mut self) -> &'static mut [Segment<'static>] {
        unsafe { core::slice::from_raw_parts_mut(self.segments.as_mut_ptr(), self.segments.len()) }
    }

    pub fn complete(self, dst: Option<&mut [u8]>) {
        if matches!(
            self.direction,
            DmaDirection::FromDevice | DmaDirection::Bidirectional
        ) && let Some(dst) = dst
        {
            self.buffer.copy_from_device_to_slice(dst);
        }
    }
}

pub enum RuntimeDmaBuffer {
    Legacy(DmaBufferGuard),
    Owned(CompletedDma),
}

impl RuntimeDmaBuffer {
    pub fn complete(self, dst: Option<&mut [u8]>) {
        match self {
            Self::Legacy(guard) => guard.complete(dst),
            Self::Owned(buffer) => {
                if let Some(dst) = dst {
                    buffer.copy_from_device_to_slice(dst);
                }
            }
        }
    }
}

pub fn new_owned_dma_buffer(
    dma: &DeviceDma,
    len: usize,
    align: usize,
    direction: DmaDirection,
    src: Option<&[u8]>,
) -> Result<CpuDmaBuffer, BlkError> {
    let mut buffer = CpuDmaBuffer::new_zero(
        dma,
        NonZeroUsize::new(len.max(1)).expect("len.max(1) is non-zero"),
        align.max(1),
        direction,
    )
    .map_err(BlkError::from)?;
    match direction {
        DmaDirection::FromDevice => {}
        DmaDirection::ToDevice | DmaDirection::Bidirectional => {
            if let Some(src) = src {
                buffer.copy_to_device_from_slice(src);
            }
        }
    }
    Ok(buffer)
}

#[cfg(test)]
pub struct VecDmaOp;

#[cfg(test)]
impl DmaOp for VecDmaOp {
    fn page_size(&self) -> usize {
        4096
    }

    unsafe fn alloc_contiguous(
        &self,
        _constraints: dma_api::DmaConstraints,
        layout: core::alloc::Layout,
    ) -> Option<dma_api::DmaAllocHandle> {
        alloc_dma_handle(layout)
    }

    unsafe fn dealloc_contiguous(&self, handle: dma_api::DmaAllocHandle) {
        dealloc_dma_handle(handle);
    }

    unsafe fn alloc_coherent(
        &self,
        _constraints: dma_api::DmaConstraints,
        layout: core::alloc::Layout,
    ) -> Option<dma_api::DmaAllocHandle> {
        alloc_dma_handle(layout)
    }

    unsafe fn dealloc_coherent(&self, handle: dma_api::DmaAllocHandle) {
        dealloc_dma_handle(handle);
    }

    unsafe fn map_streaming(
        &self,
        _constraints: dma_api::DmaConstraints,
        addr: core::ptr::NonNull<u8>,
        size: core::num::NonZeroUsize,
        _direction: DmaDirection,
    ) -> Result<dma_api::DmaMapHandle, dma_api::DmaError> {
        let layout = core::alloc::Layout::from_size_align(size.get(), 1)?;
        Ok(unsafe {
            dma_api::DmaMapHandle::new(
                addr,
                dma_api::DmaAddr::from(addr.as_ptr() as u64),
                layout,
                None,
            )
        })
    }

    unsafe fn unmap_streaming(&self, _handle: dma_api::DmaMapHandle) {}
}

#[cfg(test)]
fn alloc_dma_handle(layout: core::alloc::Layout) -> Option<dma_api::DmaAllocHandle> {
    let layout = non_empty_layout(layout);
    let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
    let ptr = core::ptr::NonNull::new(ptr)?;
    Some(unsafe {
        dma_api::DmaAllocHandle::new(ptr, dma_api::DmaAddr::from(ptr.as_ptr() as u64), layout)
    })
}

#[cfg(test)]
fn dealloc_dma_handle(handle: dma_api::DmaAllocHandle) {
    unsafe { std::alloc::dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
}

#[cfg(test)]
fn non_empty_layout(layout: core::alloc::Layout) -> core::alloc::Layout {
    if layout.size() == 0 {
        core::alloc::Layout::from_size_align(1, layout.align()).expect("valid non-empty layout")
    } else {
        layout
    }
}

#[cfg(test)]
pub static VEC_DMA_OP: VecDmaOp = VecDmaOp;
