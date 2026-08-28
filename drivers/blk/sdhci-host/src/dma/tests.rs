use alloc::alloc::{alloc_zeroed, dealloc};
use core::{
    alloc::Layout,
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering},
};

use super::*;

fn empty_table() -> [Adma2Desc32; ADMA2_DESC_COUNT] {
    [Adma2Desc32 {
        attr: 0,
        length: 0,
        address: 0,
    }; ADMA2_DESC_COUNT]
}

fn empty_table64() -> [Adma2Desc64; ADMA2_DESC_COUNT] {
    [Adma2Desc64::default(); ADMA2_DESC_COUNT]
}

#[test]
fn single_descriptor_for_small_buffer() {
    let mut table = empty_table();
    let n = build_descriptors(&mut table, 0x1000_0000, 512, Phase::DataRead).unwrap();
    assert_eq!(n, 1);
    assert_eq!(table[0].length, 512);
    assert_eq!(table[0].address, 0x1000_0000);
    // Valid + End + Tran action
    assert_eq!(
        table[0].attr,
        ADMA2_ATTR_VALID | ADMA2_ATTR_END | ADMA2_ATTR_ACT_TRAN
    );
}

#[test]
fn splits_across_max_chunk() {
    let mut table = empty_table();
    let total = ADMA2_MAX_PER_DESC + 4096;
    let n = build_descriptors(&mut table, 0x2000_0000, total, Phase::DataRead).unwrap();
    assert_eq!(n, 2);
    assert_eq!(table[0].length as usize, ADMA2_MAX_PER_DESC);
    // first descriptor must NOT have END
    assert!(table[0].attr & ADMA2_ATTR_END == 0);
    // second descriptor covers the tail and has END
    assert_eq!(table[1].length, 4096);
    assert!(table[1].attr & ADMA2_ATTR_END != 0);
    assert_eq!(table[1].address, 0x2000_0000 + ADMA2_MAX_PER_DESC as u32);
}

#[test]
fn splits_at_dwcmshc_128m_boundary() {
    let mut table = empty_table();
    let base = DWC_MSHC_ADMA_BOUNDARY as u64 - 1024;
    let n = build_descriptors(&mut table, base, 4096, Phase::DataRead).unwrap();

    assert_eq!(n, 2);
    assert_eq!(table[0].length, 1024);
    assert_eq!(table[0].address, base as u32);
    assert!(table[0].attr & ADMA2_ATTR_END == 0);
    assert_eq!(table[1].length, 3072);
    assert_eq!(table[1].address, DWC_MSHC_ADMA_BOUNDARY as u32);
    assert!(table[1].attr & ADMA2_ATTR_END != 0);
}

#[test]
fn rejects_64bit_bus_address() {
    let mut table = empty_table();
    let err = build_descriptors(&mut table, 0x1_0000_0000, 512, Phase::DataRead).unwrap_err();
    assert!(matches!(err, Error::BadResponse(_)));
}

#[test]
fn descriptor64_preserves_address_above_4gib() {
    let mut table = empty_table64();
    let base = 0x1_0000_1000;
    let written = build_descriptors64(&mut table, base, 512).unwrap();

    assert_eq!(written, 1);
    assert_eq!(table[0].address_low, 0x1000);
    assert_eq!(table[0].address_high, 1);
    assert_eq!(table[0].length, 512);
    assert_ne!(table[0].attr & ADMA2_ATTR_END, 0);
}

#[test]
fn rejects_unaligned_bus_address() {
    let mut table = empty_table();
    let err = build_descriptors(&mut table, 0x1000_0002, 512, Phase::DataRead).unwrap_err();
    assert!(matches!(err, Error::Misaligned));
}

#[test]
fn rejects_transfer_past_32bit_dma_mask() {
    let mut table = empty_table();
    let err = build_descriptors(&mut table, 0xffff_ff00, 512, Phase::DataRead).unwrap_err();
    assert!(matches!(err, Error::BadResponse(_)));
}

#[test]
fn rejects_zero_length() {
    let mut table = empty_table();
    let err = build_descriptors(&mut table, 0, 0, Phase::DataRead).unwrap_err();
    assert!(matches!(err, Error::Misaligned));
}

#[test]
fn sdhci_dma_read_plan_rejects_non_block_sized_buffers() {
    let size = core::num::NonZeroUsize::new(513).unwrap();
    assert_eq!(dma_read_block_count(size), Err(Error::Misaligned));
}

#[test]
fn sdhci_dma_read_plan_reports_block_count() {
    let size = core::num::NonZeroUsize::new(1024).unwrap();
    assert_eq!(dma_read_block_count(size), Ok(2));
}

#[test]
fn sdhci_dma_write_plan_rejects_non_block_sized_buffers() {
    let size = core::num::NonZeroUsize::new(513).unwrap();
    assert_eq!(dma_write_block_count(size), Err(Error::Misaligned));
}

#[test]
fn block_request_slot_rejects_second_request_until_completed() {
    let mut slot = BlockRequestSlot::default();
    let first = slot
        .start(BlockTransferMode::Dma, BlockTransferDirection::Read)
        .unwrap();

    assert_eq!(
        slot.start(BlockTransferMode::Dma, BlockTransferDirection::Read),
        Err(Error::UnsupportedCommand)
    );
    assert_eq!(
        slot.complete(RequestId::new(usize::from(first) + 1)),
        Err(Error::InvalidArgument)
    );
    assert_eq!(slot.complete(first), Ok(()));
    assert!(
        slot.start(BlockTransferMode::Dma, BlockTransferDirection::Read)
            .is_ok()
    );
}

#[test]
fn block_request_can_cross_queue_thread_boundary() {
    fn assert_send<T: Send>() {}

    assert_send::<BlockRequest>();
    assert_send::<BlockRequestSlot>();
}

#[test]
fn aborted_owned_dma_is_returned_after_controller_quiesce() {
    #[repr(align(4))]
    struct FakeRegisters([u8; 0x100]);

    let mut registers = FakeRegisters([0; 0x100]);
    let base = NonNull::new(registers.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.reset_auto_complete = true;

    let mut slot = BlockRequestSlot::default();
    let id = slot
        .start(BlockTransferMode::Dma, BlockTransferDirection::Write)
        .unwrap();
    let dma = test_device_dma();
    let prepared = CpuDmaBuffer::new_zero(
        &dma,
        NonZeroUsize::new(512).unwrap(),
        512,
        DmaDirection::ToDevice,
    )
    .unwrap()
    .prepare_for_device();
    let request = BlockRequest {
        inner: BlockRequestKind::Write {
            id,
            buffer: DmaRequestBuffer::Owned(unsafe { prepared.into_in_flight() }),
            cmd_index: 24,
            phase: Phase::DataWrite,
            stage: BlockRequestStage::Data,
            stop_after_complete: false,
            response: None,
        },
    };
    let mut request = Some(request);

    host.abort_block_request(&mut request, id, &mut slot)
        .unwrap();

    assert!(request.is_none());
    let completed = slot
        .take_completed_dma()
        .expect("abort must return the owned DMA token after quiesce");
    assert_eq!(completed.len().get(), 512);
    drop(completed);
    assert_eq!(TEST_DMA_DEALLOCATIONS.load(Ordering::SeqCst), 1);
}

struct TestDma;

static TEST_DMA: TestDma = TestDma;
static TEST_DMA_DEALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

impl dma_api::DmaOp for TestDma {
    fn page_size(&self) -> usize {
        4096
    }

    unsafe fn alloc_contiguous(
        &self,
        _constraints: dma_api::DmaConstraints,
        layout: Layout,
    ) -> Option<dma_api::DmaAllocHandle> {
        let ptr = NonNull::new(unsafe { alloc_zeroed(layout) })?;
        Some(unsafe {
            dma_api::DmaAllocHandle::new(ptr, ptr, (ptr.as_ptr() as usize as u64).into(), layout)
        })
    }

    unsafe fn dealloc_contiguous(&self, handle: dma_api::DmaAllocHandle) {
        TEST_DMA_DEALLOCATIONS.fetch_add(1, Ordering::SeqCst);
        unsafe { dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
    }

    unsafe fn alloc_coherent(
        &self,
        constraints: dma_api::DmaConstraints,
        layout: Layout,
    ) -> Option<dma_api::DmaAllocHandle> {
        unsafe { self.alloc_contiguous(constraints, layout) }
    }

    unsafe fn dealloc_coherent(
        &self,
        handle: dma_api::DmaAllocHandle,
    ) -> Result<(), dma_api::DmaError> {
        unsafe { self.dealloc_contiguous(handle) };
        Ok(())
    }

    unsafe fn map_streaming(
        &self,
        _constraints: dma_api::DmaConstraints,
        address: NonNull<u8>,
        size: NonZeroUsize,
        _direction: DmaDirection,
    ) -> Result<dma_api::DmaMapHandle, dma_api::DmaError> {
        let layout =
            Layout::from_size_align(size.get(), 1).map_err(dma_api::DmaError::LayoutError)?;
        Ok(unsafe {
            dma_api::DmaMapHandle::new(
                address,
                (address.as_ptr() as usize as u64).into(),
                layout,
                None,
            )
        })
    }

    unsafe fn unmap_streaming(&self, _handle: dma_api::DmaMapHandle) {}
}

fn test_device_dma() -> DeviceDma {
    DeviceDma::new(
        dma_api::DmaDeviceInfo::new(
            dma_api::DmaDomainId::Direct,
            dma_api::DmaCoherency::NonCoherent,
            dma_api::DmaConstraints::new(u64::MAX),
        ),
        &TEST_DMA,
    )
}
