use core::{alloc::Layout, num::NonZeroUsize, ptr::NonNull};
use std::alloc::{alloc_zeroed, dealloc};

use ramdisk::RamDisk;
use rdif_block::{
    BlkError, InlineExecuteQueue, OwnedRequest, RequestFlags, RequestId, RequestOp,
    dma_api::{
        CpuDmaBuffer, DeviceDma, DmaAllocHandle, DmaConstraints, DmaDirection, DmaError,
        DmaMapHandle, DmaOp,
    },
};

const RAMDISK_SOURCE: &str = include_str!("../src/lib.rs");

#[test]
fn inline_execution_returns_the_owned_request_on_success_and_error() {
    let mut disk = RamDisk::new(16, 8);
    let mut queue = disk
        .take_inline_queue()
        .expect("the inline queue must own the ramdisk storage");
    assert!(
        disk.take_inline_queue().is_none(),
        "ramdisk storage must have exactly one inline owner"
    );

    let write_buffer = dma_buffer(DmaDirection::ToDevice, 0xa5);
    let write_cpu_pointer = write_buffer.cpu_ptr();
    let write_dma_address = write_buffer.dma_addr();
    let write = queue.execute_owned(request(RequestOp::Write, 3, write_buffer));

    assert_eq!(write.id, RequestId::INLINE);
    assert_eq!(write.result, Ok(()));
    let returned_write_buffer = write
        .request
        .data
        .as_ref()
        .expect("inline success must return the submitted buffer");
    assert_eq!(returned_write_buffer.cpu_ptr(), write_cpu_pointer);
    assert_eq!(returned_write_buffer.dma_addr(), write_dma_address);

    let invalid_buffer = dma_buffer(DmaDirection::FromDevice, 0);
    let invalid_cpu_pointer = invalid_buffer.cpu_ptr();
    let invalid_dma_address = invalid_buffer.dma_addr();
    let invalid = queue.execute_owned(request(RequestOp::Read, 8, invalid_buffer));

    assert_eq!(invalid.id, RequestId::INLINE);
    assert_eq!(invalid.result, Err(BlkError::InvalidBlockIndex(8)));
    let returned_invalid_buffer = invalid
        .request
        .data
        .as_ref()
        .expect("inline error must return the submitted buffer");
    assert_eq!(returned_invalid_buffer.cpu_ptr(), invalid_cpu_pointer);
    assert_eq!(returned_invalid_buffer.dma_addr(), invalid_dma_address);
}

#[test]
fn v013_inline_queue_exposes_no_irq_or_queued_request_surface() {
    let definition_start = RAMDISK_SOURCE
        .find("pub struct RamDiskInlineQueue")
        .expect("ramdisk must expose a typed v0.13 inline queue");
    let implementation_start = RAMDISK_SOURCE[definition_start..]
        .find("impl InlineExecuteQueue for RamDiskInlineQueue")
        .map(|offset| definition_start + offset)
        .expect("the typed queue must implement InlineExecuteQueue");
    let definition = &RAMDISK_SOURCE[definition_start..implementation_start];

    for forbidden in ["irq", "waiter", "RequestId", "SubmitOutcome", "Queued"] {
        assert!(
            !definition.contains(forbidden),
            "inline queue state must not contain asynchronous concept `{forbidden}`"
        );
    }
    assert!(!RAMDISK_SOURCE.contains("impl IQueue for RamDiskInlineQueue"));
}

fn request(op: RequestOp, lba: u64, buffer: CpuDmaBuffer) -> OwnedRequest {
    OwnedRequest {
        op,
        lba,
        block_count: 1,
        data: Some(buffer),
        flags: RequestFlags::NONE,
    }
}

fn dma_buffer(direction: DmaDirection, fill: u8) -> CpuDmaBuffer {
    let dma = DeviceDma::new_legacy(u64::MAX, &TEST_DMA);
    let mut buffer = CpuDmaBuffer::new_zero(
        &dma,
        NonZeroUsize::new(16).expect("test buffer is non-zero"),
        16,
        direction,
    )
    .expect("test DMA allocation must succeed");
    // SAFETY: this freshly allocated buffer has never been device-owned.
    unsafe { buffer.as_mut_slice_cpu() }.fill(fill);
    buffer
}

struct TestDma;

impl DmaOp for TestDma {
    fn page_size(&self) -> usize {
        4096
    }

    unsafe fn alloc_contiguous(
        &self,
        _constraints: DmaConstraints,
        layout: Layout,
    ) -> Option<DmaAllocHandle> {
        let pointer = NonNull::new(unsafe { alloc_zeroed(layout) })?;
        Some(unsafe { DmaAllocHandle::new(pointer, (pointer.as_ptr() as u64).into(), layout) })
    }

    unsafe fn dealloc_contiguous(&self, handle: DmaAllocHandle) {
        unsafe { dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
    }

    unsafe fn alloc_coherent(
        &self,
        constraints: DmaConstraints,
        layout: Layout,
    ) -> Option<DmaAllocHandle> {
        unsafe { self.alloc_contiguous(constraints, layout) }
    }

    unsafe fn dealloc_coherent(&self, handle: DmaAllocHandle) {
        unsafe { self.dealloc_contiguous(handle) };
    }

    unsafe fn map_streaming(
        &self,
        _constraints: DmaConstraints,
        address: NonNull<u8>,
        size: NonZeroUsize,
        _direction: DmaDirection,
    ) -> Result<DmaMapHandle, DmaError> {
        let layout = Layout::from_size_align(size.get(), 1)?;
        Ok(unsafe { DmaMapHandle::new(address, (address.as_ptr() as u64).into(), layout, None) })
    }

    unsafe fn unmap_streaming(&self, _handle: DmaMapHandle) {}
}

static TEST_DMA: TestDma = TestDma;
