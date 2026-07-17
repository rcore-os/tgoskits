struct CountingDma {
    allocations: AtomicUsize,
    deallocations: AtomicUsize,
}

impl CountingDma {
    const fn new() -> Self {
        Self {
            allocations: AtomicUsize::new(0),
            deallocations: AtomicUsize::new(0),
        }
    }
}

impl dma_api::DmaOp for CountingDma {
    unsafe fn alloc_contiguous(
        &self,
        constraints: dma_api::DmaConstraints,
        layout: core::alloc::Layout,
    ) -> Option<dma_api::DmaAllocHandle> {
        self.allocations.fetch_add(1, Ordering::Relaxed);
        unsafe { dma_api::DmaOp::alloc_contiguous(&TEST_DMA, constraints, layout) }
    }

    unsafe fn dealloc_contiguous(&self, handle: dma_api::DmaAllocHandle) {
        self.deallocations.fetch_add(1, Ordering::Relaxed);
        unsafe { dma_api::DmaOp::dealloc_contiguous(&TEST_DMA, handle) };
    }

    unsafe fn alloc_coherent(
        &self,
        constraints: dma_api::DmaConstraints,
        layout: core::alloc::Layout,
    ) -> Option<dma_api::DmaAllocHandle> {
        self.allocations.fetch_add(1, Ordering::Relaxed);
        unsafe { dma_api::DmaOp::alloc_coherent(&TEST_DMA, constraints, layout) }
    }

    unsafe fn dealloc_coherent(&self, handle: dma_api::DmaAllocHandle) {
        self.deallocations.fetch_add(1, Ordering::Relaxed);
        unsafe { dma_api::DmaOp::dealloc_coherent(&TEST_DMA, handle) };
    }

    unsafe fn map_streaming(
        &self,
        constraints: dma_api::DmaConstraints,
        addr: NonNull<u8>,
        size: NonZeroUsize,
        direction: DmaDirection,
    ) -> Result<dma_api::DmaMapHandle, dma_api::DmaError> {
        unsafe { dma_api::DmaOp::map_streaming(&TEST_DMA, constraints, addr, size, direction) }
    }

    unsafe fn unmap_streaming(&self, handle: dma_api::DmaMapHandle) {
        unsafe { dma_api::DmaOp::unmap_streaming(&TEST_DMA, handle) };
    }

    fn flush(&self, addr: NonNull<u8>, size: usize) {
        dma_api::DmaOp::flush(&TEST_DMA, addr, size);
    }

    fn invalidate(&self, addr: NonNull<u8>, size: usize) {
        dma_api::DmaOp::invalidate(&TEST_DMA, addr, size);
    }

    fn flush_invalidate(&self, addr: NonNull<u8>, size: usize) {
        dma_api::DmaOp::flush_invalidate(&TEST_DMA, addr, size);
    }

    fn page_size(&self) -> usize {
        dma_api::DmaOp::page_size(&TEST_DMA)
    }
}
