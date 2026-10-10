//! Host DMA fixtures shared by the RDIF owner tests.
//!
//! The driver core owns no DMA backend, so owner tests that need real
//! move-only [`DmaBuffer`] tokens allocate them through one identity-mapped
//! host allocator.

// DMA pool locks use the same spin-operation provider as the kernel build.
extern crate ax_runtime as _;

use alloc::alloc::{alloc_zeroed, dealloc};
use core::{alloc::Layout, num::NonZeroUsize, ptr::NonNull};

use dma_api::{
    ContiguousBuffer, DeviceDma, DmaAllocHandle, DmaCoherency, DmaConstraints, DmaDeviceInfo,
    DmaDirection, DmaDomainId, DmaError, DmaMapHandle, DmaOp,
};
use rdif_eth::DmaBuffer;

/// Allocation size of every test buffer, matching the submit ring's frame size.
const TEST_BUFFER_BYTES: usize = 2048;

struct TestDma;

impl TestDma {
    unsafe fn allocate(layout: Layout) -> Option<DmaAllocHandle> {
        let pointer = NonNull::new(unsafe { alloc_zeroed(layout) })?;
        // SAFETY: the allocation satisfies `layout` and stays live until the
        // paired deallocator receives this handle.
        Some(unsafe {
            DmaAllocHandle::new(
                pointer,
                pointer,
                (pointer.as_ptr() as usize as u64).into(),
                layout,
            )
        })
    }
}

impl DmaOp for TestDma {
    fn page_size(&self) -> usize {
        4096
    }

    unsafe fn alloc_contiguous(
        &self,
        _constraints: DmaConstraints,
        layout: Layout,
    ) -> Option<DmaAllocHandle> {
        unsafe { Self::allocate(layout) }
    }

    unsafe fn dealloc_contiguous(&self, handle: DmaAllocHandle) {
        // SAFETY: handles come from `allocate` and are released exactly once.
        unsafe { dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
    }

    unsafe fn alloc_coherent(
        &self,
        _constraints: DmaConstraints,
        layout: Layout,
    ) -> Option<DmaAllocHandle> {
        unsafe { Self::allocate(layout) }
    }

    unsafe fn dealloc_coherent(&self, handle: DmaAllocHandle) -> Result<(), DmaError> {
        // SAFETY: paired with `alloc_coherent`; test buffers are host memory.
        unsafe { dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
        Ok(())
    }

    unsafe fn map_streaming(
        &self,
        _constraints: DmaConstraints,
        addr: NonNull<u8>,
        size: NonZeroUsize,
        _direction: DmaDirection,
    ) -> Result<DmaMapHandle, DmaError> {
        let layout = Layout::from_size_align(size.get(), 1)?;
        // SAFETY: the backing allocation outlives the returned mapping handle.
        Ok(
            unsafe {
                DmaMapHandle::new(addr, (addr.as_ptr() as usize as u64).into(), layout, None)
            },
        )
    }

    unsafe fn unmap_streaming(&self, _handle: DmaMapHandle) {}
}

/// Identity-mapped direct host memory: one pointer is both CPU and device
/// address, so a test buffer needs no explicit streaming synchronization.
static TEST_DMA: TestDma = TestDma;

fn allocate() -> ContiguousBuffer {
    let dma = DeviceDma::new(
        DmaDeviceInfo::new(
            DmaDomainId::Direct,
            DmaCoherency::Coherent,
            DmaConstraints::new(u64::MAX),
        ),
        &TEST_DMA,
    );
    let pool = dma.contiguous_buffer_pool(
        Layout::from_size_align(TEST_BUFFER_BYTES, 64).expect("test layout is valid"),
        DmaDirection::Bidirectional,
        1,
    );
    match pool.alloc() {
        Ok(buffer) => buffer,
        Err(_) => panic!("test DMA allocation must succeed"),
    }
}

/// Allocates one zero-filled move-only buffer of `len` bytes.
///
/// The returned token owns its allocation for as long as it stays outside a
/// queue, so a test can move it through submit, completion and reclaim paths.
pub(crate) fn dma_buffer(len: usize) -> DmaBuffer {
    match DmaBuffer::new(allocate(), len) {
        Ok(buffer) => buffer,
        Err(_) => panic!("test DMA buffer length must fit its allocation"),
    }
}
