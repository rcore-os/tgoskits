use super::*;

#[test]
fn idmac_interrupt_mask_matches_linux_phytium_error_contract() {
    assert_eq!(
        IDSTS_INT_ENABLE_MASK,
        crate::MCI_IDSTS_FATAL_BUS_ERROR | (1 << 4) | IDSTS_NORMAL_SUMMARY | IDSTS_ABNORMAL_SUMMARY
    );
}

use core::ptr::NonNull;

use ::alloc::{alloc, boxed::Box};
use sdmmc_protocol::{block::BlockProgress, sdio::host::SdMmcIrqHandle};

use crate::regs::RIntSts;

#[repr(align(512))]
struct AlignedBlock([u8; BLOCK_SIZE]);

struct NoopDmaBuffer;

impl NoopDmaBuffer {
    fn progress() -> DmaProgress {
        let dma = DeviceDma::new(
            dma_api::DmaDeviceInfo::new(
                dma_api::DmaDomainId::Direct,
                dma_api::DmaCoherency::NonCoherent,
                dma_api::DmaConstraints::new(u64::MAX),
            ),
            &TEST_DMA,
        );
        let buffer = CpuDmaBuffer::new_zero(
            &dma,
            NonZeroUsize::new(BLOCK_SIZE).unwrap(),
            BLOCK_SIZE,
            DmaDirection::FromDevice,
        )
        .unwrap()
        .prepare_for_device();
        let buffer = unsafe { buffer.into_in_flight() };
        let backing = Box::leak(Box::new(AlignedBlock([0u8; BLOCK_SIZE])));
        let readback = Some((NonNull::from(&mut backing.0[0]), BLOCK_SIZE));
        let buffer = DmaRequestBuffer::Bounce { buffer, readback };
        DmaProgress {
            buffer,
            data_done: false,
        }
    }
}

struct TestDma;
static TEST_DMA: TestDma = TestDma;

impl dma_api::DmaOp for TestDma {
    unsafe fn alloc_contiguous(
        &self,
        _constraints: dma_api::DmaConstraints,
        layout: core::alloc::Layout,
    ) -> Option<dma_api::DmaAllocHandle> {
        let ptr = unsafe { alloc::alloc_zeroed(layout) };
        let ptr = NonNull::new(ptr)?;
        Some(unsafe {
            dma_api::DmaAllocHandle::new(ptr, ptr, (ptr.as_ptr() as u64).into(), layout)
        })
    }

    unsafe fn dealloc_contiguous(&self, handle: dma_api::DmaAllocHandle) {
        unsafe { alloc::dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
    }

    unsafe fn alloc_coherent(
        &self,
        _constraints: dma_api::DmaConstraints,
        layout: core::alloc::Layout,
    ) -> Option<dma_api::DmaAllocHandle> {
        let ptr = unsafe { alloc::alloc_zeroed(layout) };
        let ptr = NonNull::new(ptr)?;
        Some(unsafe {
            dma_api::DmaAllocHandle::new(ptr, ptr, (ptr.as_ptr() as u64).into(), layout)
        })
    }

    unsafe fn dealloc_coherent(
        &self,
        handle: dma_api::DmaAllocHandle,
    ) -> Result<(), dma_api::DmaError> {
        unsafe { alloc::dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
        Ok(())
    }

    unsafe fn map_streaming(
        &self,
        constraints: dma_api::DmaConstraints,
        addr: NonNull<u8>,
        size: NonZeroUsize,
        _direction: DmaDirection,
    ) -> Result<dma_api::DmaMapHandle, dma_api::DmaError> {
        let layout = core::alloc::Layout::from_size_align(size.get(), constraints.align.max(1))?;
        Ok(
            unsafe {
                dma_api::DmaMapHandle::new(addr, (addr.as_ptr() as u64).into(), layout, None)
            },
        )
    }

    unsafe fn unmap_streaming(&self, _handle: dma_api::DmaMapHandle) {}

    fn flush(&self, _addr: NonNull<u8>, _size: usize) {}
    fn invalidate(&self, _addr: NonNull<u8>, _size: usize) {}
    fn flush_invalidate(&self, _addr: NonNull<u8>, _size: usize) {}
    fn page_size(&self) -> usize {
        4096
    }
}

const RINTSTS_WORD: usize = 17;
const CTRL_WORD: usize = 0;
const BMOD_WORD: usize = 32;
const PLDMND_WORD: usize = 33;
const DBADDRL_WORD: usize = 34;
const IDSTS_WORD: usize = 36;

fn host_from_words(words: &mut [u32]) -> PhytiumMci {
    let base = NonNull::new(words.as_mut_ptr().cast()).unwrap();
    unsafe { PhytiumMci::new(base) }
}

#[test]
fn idmac_start_preserves_bus_mode_and_enables_fixed_burst() {
    let mut mmio = [0u32; 1024];
    mmio[BMOD_WORD] = 0x200;
    let host = host_from_words(&mut mmio);

    host.program_idmac_registers(0x1_8000_0000);
    host.kick_idmac();

    assert_eq!(
        mmio[BMOD_WORD],
        0x200 | BMOD_FIXED_BURST | BMOD_IDMAC_ENABLE
    );
    let ctrl = crate::regs::Ctrl::from_bits(mmio[CTRL_WORD]);
    assert!(!ctrl.dma_enable());
    assert!(ctrl.use_internal_dmac());
    assert!(!ctrl.int_enable());
    assert_eq!(mmio[PLDMND_WORD], 1);
    assert_eq!(mmio[DBADDRL_WORD], 0x8000_0000);
    assert_eq!(mmio[DBADDRL_WORD + 1], 1);
}

#[test]
fn idmac_reset_disables_stale_bus_mode_and_descriptor_base() {
    let mut mmio = [0u32; 1024];
    mmio[BMOD_WORD] = 0x282;
    mmio[DBADDRL_WORD] = 0x1234_0000;
    mmio[DBADDRL_WORD + 1] = 1;
    let host = host_from_words(&mut mmio);

    host.start_idmac_reset();

    assert_eq!(mmio[BMOD_WORD], crate::host::BMOD_SOFTWARE_RESET);
    assert_eq!(mmio[DBADDRL_WORD], 0);
    assert_eq!(mmio[DBADDRL_WORD + 1], 0);
}

#[test]
fn controller_data_over_completes_without_an_idmac_ri_bit() {
    let mut mmio = [0u32; 1024];
    let mut host = host_from_words(&mut mmio);
    host.enable_completion_irq();
    host.irq.state.begin_request();
    let mut request = Some(BlockRequest {
        inner: BlockRequestKind::DmaRead {
            id: RequestId::new(3),
            progress: NoopDmaBuffer::progress(),
            cmd_index: 17,
            phase: Phase::DataRead,
            stage: BlockRequestStage::Data,
            stop_after_complete: false,
            response: Some(Response::Empty),
        },
    });

    unsafe {
        mmio.as_mut_ptr()
            .add(RINTSTS_WORD)
            .write_volatile(RIntSts::new().with_data_transfer_over(true).into_bits())
    };
    assert_eq!(
        host.irq_endpoint().handle_irq(),
        crate::Event::TransferComplete
    );

    assert_eq!(
        host.consume_dma_completion(&mut request, 17, Phase::DataRead)
            .unwrap(),
        BlockProgress::Complete
    );
}

#[test]
fn idmac_read_completes_when_idmac_and_data_done_arrive_separately() {
    let mut mmio = [0u32; 1024];
    let mut host = host_from_words(&mut mmio);
    host.enable_completion_irq();
    host.irq.state.begin_request();
    let mut request = Some(BlockRequest {
        inner: BlockRequestKind::DmaRead {
            id: RequestId::new(2),
            progress: NoopDmaBuffer::progress(),
            cmd_index: 17,
            phase: Phase::DataRead,
            stage: BlockRequestStage::Data,
            stop_after_complete: false,
            response: Some(Response::Empty),
        },
    });

    unsafe { mmio.as_mut_ptr().add(IDSTS_WORD).write_volatile(1 << 1) };
    assert_eq!(host.irq_endpoint().handle_irq(), crate::Event::None);
    assert_eq!(
        host.consume_dma_completion(&mut request, 17, Phase::DataRead)
            .unwrap(),
        BlockProgress::Pending
    );

    unsafe {
        mmio.as_mut_ptr()
            .add(RINTSTS_WORD)
            .write_volatile(RIntSts::new().with_data_transfer_over(true).into_bits())
    };
    assert_eq!(
        host.irq_endpoint().handle_irq(),
        crate::Event::TransferComplete
    );
    assert_eq!(
        host.consume_dma_completion(&mut request, 17, Phase::DataRead)
            .unwrap(),
        BlockProgress::Complete
    );
}

#[test]
fn stop_completion_consumes_fast_cmd12_irq_without_second_wakeup() {
    let mut mmio = [0u32; 1024];
    let mut host = host_from_words(&mut mmio);
    let mut slot = BlockRequestSlot::default();
    let id = slot
        .start(BlockTransferMode::Dma, BlockTransferDirection::Write)
        .unwrap();
    let mut progress = NoopDmaBuffer::progress();
    progress.data_done = true;
    let mut request = Some(BlockRequest {
        inner: BlockRequestKind::DmaWrite {
            id,
            progress,
            cmd_index: 25,
            phase: Phase::DataWrite,
            stage: BlockRequestStage::Stop,
            stop_after_complete: true,
            response: Some(Response::Empty),
        },
    });
    host.command_state = crate::command::CommandState::WaitingStart {
        cmd: CMD12,
        polls: 0,
    };
    host.irq.state.begin_request();
    let generation = host.irq.state.generation();
    host.irq
        .state
        .cache_if_current(generation, crate::MCI_INT_COMMAND_DONE, 0);

    assert!(matches!(
        host.advance_block_request_response(
            &mut request,
            id,
            &mut slot,
            sdmmc_host::ProgressCause::AcknowledgedIrq,
        )
        .unwrap(),
        DataCommandProgress::Complete(Response::Empty)
    ));
    assert!(request.is_none());
    assert_eq!(slot.state(), BlockTransferState::Idle);
}

#[test]
fn request_slot_returns_completed_owned_dma_once() {
    let dma = DeviceDma::new(
        dma_api::DmaDeviceInfo::new(
            dma_api::DmaDomainId::Direct,
            dma_api::DmaCoherency::NonCoherent,
            dma_api::DmaConstraints::new(u64::MAX),
        ),
        &TEST_DMA,
    );
    let buffer = dma_api::CpuDmaBuffer::new_zero(
        &dma,
        NonZeroUsize::new(BLOCK_SIZE).unwrap(),
        BLOCK_SIZE,
        DmaDirection::FromDevice,
    )
    .unwrap()
    .prepare_for_device();
    let in_flight = unsafe { buffer.into_in_flight() };
    let completed = DmaRequestBuffer::Owned(in_flight).complete(true).unwrap();
    let mut slot = BlockRequestSlot::default();
    let id = slot
        .start(BlockTransferMode::Dma, BlockTransferDirection::Read)
        .unwrap();

    slot.complete_with_dma(id, Some(completed)).unwrap();

    assert!(slot.take_completed_dma().is_some());
    assert!(slot.take_completed_dma().is_none());
}
