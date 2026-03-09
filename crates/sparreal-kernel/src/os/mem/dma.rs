use alloc::alloc::handle_alloc_error;
use core::{alloc::Layout, num::NonZeroUsize, ptr::NonNull};

pub use dma_api::{DmaDirection, DmaError, DmaHandle, DmaMapHandle, DmaOp};

use super::{
    address::{PhysAddr, VirtAddr},
    kernel_memory_allocator, page_size,
};

pub struct KernelDmaOp;

static KERNEL_DMA_OP: KernelDmaOp = KernelDmaOp;

pub fn kernel_dma_op() -> &'static KernelDmaOp {
    &KERNEL_DMA_OP
}

impl DmaOp for KernelDmaOp {
    fn page_size(&self) -> usize {
        page_size()
    }

    unsafe fn map_single(
        &self,
        dma_mask: u64,
        addr: NonNull<u8>,
        size: NonZeroUsize,
        align: usize,
        _direction: DmaDirection,
    ) -> Result<DmaMapHandle, DmaError> {
        let layout = Layout::from_size_align(size.get(), align.max(1))?;
        let phys: PhysAddr = VirtAddr::from(addr).into();
        let dma_addr = phys.raw() as u64;

        if dma_addr > dma_mask || !dma_addr.is_multiple_of(align.max(1) as u64) {
            return Err(DmaError::AlignMismatch {
                required: align.max(1),
                address: dma_addr.into(),
            });
        }

        Ok(unsafe { DmaMapHandle::new(addr, dma_addr.into(), layout, None) })
    }

    unsafe fn unmap_single(&self, _handle: DmaMapHandle) {}

    unsafe fn alloc_coherent(&self, dma_mask: u64, layout: Layout) -> Option<DmaHandle> {
        let ptr = unsafe { kernel_memory_allocator().alloc_with_mask(layout, dma_mask) };
        let ptr = NonNull::new(ptr)?;

        unsafe {
            ptr.as_ptr().write_bytes(0, layout.size());
        }

        let phys: PhysAddr = VirtAddr::from(ptr).into();
        let dma_addr = phys.raw() as u64;
        if dma_addr > dma_mask || !dma_addr.is_multiple_of(layout.align() as u64) {
            unsafe { kernel_memory_allocator().dealloc_raw(ptr.as_ptr(), layout) };
            return None;
        }

        Some(unsafe { DmaHandle::new(ptr, dma_addr.into(), layout) })
    }

    unsafe fn dealloc_coherent(&self, handle: DmaHandle) {
        unsafe { kernel_memory_allocator().dealloc_raw(handle.as_ptr().as_ptr(), handle.layout()) }
    }
}

pub fn alloc_with_mask(layout: Layout, dma_mask: u64) -> NonNull<u8> {
    let ptr = unsafe { kernel_memory_allocator().alloc_with_mask(layout, dma_mask) };
    NonNull::new(ptr).unwrap_or_else(|| handle_alloc_error(layout))
}
