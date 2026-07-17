use core::{alloc::Layout, num::NonZeroUsize, ptr::NonNull};

use dma_api::{DmaAllocHandle, DmaConstraints, DmaDirection, DmaError, DmaMapHandle, DmaOp};

pub(crate) static TEST_DMA: TestDma = TestDma;

pub(crate) struct TestDma;

impl DmaOp for TestDma {
    fn page_size(&self) -> usize {
        4096
    }

    unsafe fn alloc_contiguous(
        &self,
        _constraints: DmaConstraints,
        layout: Layout,
    ) -> Option<DmaAllocHandle> {
        allocate(layout)
    }

    unsafe fn dealloc_contiguous(&self, handle: DmaAllocHandle) {
        unsafe { std::alloc::dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
    }

    unsafe fn alloc_coherent(
        &self,
        _constraints: DmaConstraints,
        layout: Layout,
    ) -> Option<DmaAllocHandle> {
        allocate(layout)
    }

    unsafe fn dealloc_coherent(&self, handle: DmaAllocHandle) {
        unsafe { std::alloc::dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
    }

    unsafe fn map_streaming(
        &self,
        _constraints: DmaConstraints,
        addr: NonNull<u8>,
        size: NonZeroUsize,
        _direction: DmaDirection,
    ) -> Result<DmaMapHandle, DmaError> {
        let layout = Layout::from_size_align(size.get(), 1)?;
        Ok(unsafe {
            DmaMapHandle::new(
                addr,
                dma_api::DmaAddr::from(addr.as_ptr() as u64),
                layout,
                None,
            )
        })
    }

    unsafe fn unmap_streaming(&self, _handle: DmaMapHandle) {}
}

fn allocate(layout: Layout) -> Option<DmaAllocHandle> {
    let ptr = NonNull::new(unsafe { std::alloc::alloc_zeroed(layout) })?;
    Some(unsafe { DmaAllocHandle::new(ptr, dma_api::DmaAddr::from(ptr.as_ptr() as u64), layout) })
}
