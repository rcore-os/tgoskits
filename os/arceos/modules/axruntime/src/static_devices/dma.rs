use core::{alloc::Layout, num::NonZeroUsize, ptr::NonNull};

use dma_api::{DmaError, DmaHandle, DmaMapHandle};

pub(super) struct IdentityDma;

impl dma_api::DmaOp for IdentityDma {
    fn page_size(&self) -> usize {
        4096
    }

    unsafe fn map_single(
        &self,
        _dma_mask: u64,
        addr: NonNull<u8>,
        size: NonZeroUsize,
        align: usize,
        _direction: dma_api::DmaDirection,
    ) -> Result<DmaMapHandle, DmaError> {
        let layout = Layout::from_size_align(size.get(), align.max(1))?;
        let dma_addr = (addr.as_ptr() as usize as u64).into();
        Ok(unsafe { DmaMapHandle::new(addr, dma_addr, layout, None) })
    }

    unsafe fn unmap_single(&self, _handle: DmaMapHandle) {}

    unsafe fn alloc_coherent(&self, _dma_mask: u64, layout: Layout) -> Option<DmaHandle> {
        let ptr = ax_alloc::global_allocator().alloc(layout).ok()?;
        unsafe {
            ptr.as_ptr().write_bytes(0, layout.size());
        }
        let dma_addr = (ptr.as_ptr() as usize as u64).into();
        Some(unsafe { DmaHandle::new(ptr, dma_addr, layout) })
    }

    unsafe fn dealloc_coherent(&self, handle: DmaHandle) {
        ax_alloc::global_allocator().dealloc(handle.as_ptr(), handle.layout());
    }
}

pub(super) static IDENTITY_DMA: IdentityDma = IdentityDma;
