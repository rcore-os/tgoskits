use core::{
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering},
};

use rdif_irq::{IrqCapture, IrqEndpoint};
use sdmmc_protocol::response::Response;

use super::*;
use crate::command::CommandState;

#[repr(align(4))]
struct FakeRegs([u8; 0x100]);

struct TestDma;

static TEST_DMA: TestDma = TestDma;

struct DropAuditDma;

static DROP_AUDIT_DMA: DropAuditDma = DropAuditDma;
static DROP_AUDIT_COHERENT_DEALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

impl dma_api::DmaOp for TestDma {
    fn page_size(&self) -> usize {
        4096
    }

    unsafe fn alloc_contiguous(
        &self,
        _constraints: dma_api::DmaConstraints,
        layout: core::alloc::Layout,
    ) -> Option<dma_api::DmaAllocHandle> {
        let ptr = NonNull::new(unsafe { alloc::alloc::alloc_zeroed(layout) })?;
        Some(unsafe {
            dma_api::DmaAllocHandle::new(ptr, dma_api::DmaAddr::from(0x2000_0000), layout)
        })
    }

    unsafe fn dealloc_contiguous(&self, handle: dma_api::DmaAllocHandle) {
        unsafe { alloc::alloc::dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
    }

    unsafe fn alloc_coherent(
        &self,
        constraints: dma_api::DmaConstraints,
        layout: core::alloc::Layout,
    ) -> Option<dma_api::DmaAllocHandle> {
        unsafe { self.alloc_contiguous(constraints, layout) }
    }

    unsafe fn dealloc_coherent(&self, handle: dma_api::DmaAllocHandle) {
        unsafe { self.dealloc_contiguous(handle) };
    }

    unsafe fn map_streaming(
        &self,
        _constraints: dma_api::DmaConstraints,
        _addr: NonNull<u8>,
        _size: NonZeroUsize,
        _direction: DmaDirection,
    ) -> Result<dma_api::DmaMapHandle, dma_api::DmaError> {
        Err(dma_api::DmaError::NoMemory)
    }

    unsafe fn unmap_streaming(&self, _handle: dma_api::DmaMapHandle) {}
}

impl dma_api::DmaOp for DropAuditDma {
    fn page_size(&self) -> usize {
        4096
    }

    unsafe fn alloc_contiguous(
        &self,
        _constraints: dma_api::DmaConstraints,
        layout: core::alloc::Layout,
    ) -> Option<dma_api::DmaAllocHandle> {
        let ptr = NonNull::new(unsafe { alloc::alloc::alloc_zeroed(layout) })?;
        Some(unsafe {
            dma_api::DmaAllocHandle::new(ptr, dma_api::DmaAddr::from(0x2100_0000), layout)
        })
    }

    unsafe fn dealloc_contiguous(&self, handle: dma_api::DmaAllocHandle) {
        unsafe { alloc::alloc::dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
    }

    unsafe fn alloc_coherent(
        &self,
        constraints: dma_api::DmaConstraints,
        layout: core::alloc::Layout,
    ) -> Option<dma_api::DmaAllocHandle> {
        unsafe { self.alloc_contiguous(constraints, layout) }
    }

    unsafe fn dealloc_coherent(&self, handle: dma_api::DmaAllocHandle) {
        DROP_AUDIT_COHERENT_DEALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { self.dealloc_contiguous(handle) };
    }

    unsafe fn map_streaming(
        &self,
        _constraints: dma_api::DmaConstraints,
        _addr: NonNull<u8>,
        _size: NonZeroUsize,
        _direction: DmaDirection,
    ) -> Result<dma_api::DmaMapHandle, dma_api::DmaError> {
        Err(dma_api::DmaError::NoMemory)
    }

    unsafe fn unmap_streaming(&self, _handle: dma_api::DmaMapHandle) {}
}

fn empty_table() -> [Adma2Desc32; ADMA2_DESC_COUNT] {
    [Adma2Desc32 {
        attr: 0,
        length: 0,
        address: 0,
    }; ADMA2_DESC_COUNT]
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
    let base = DWC_MSHC_ADMA_BOUNDARY - 1024;
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
fn rejects_zero_length() {
    let mut table = empty_table();
    let err = build_descriptors(&mut table, 0, 0, Phase::DataRead).unwrap_err();
    assert!(matches!(err, Error::Misaligned));
}

#[test]
fn irq_owned_busy_submit_never_reprograms_the_adma_engine() {
    const ORIGINAL_ADMA_ADDR: u32 = 0x1234_5000;

    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    let dma = DeviceDma::new_legacy(u32::MAX as u64, &TEST_DMA);
    let mut descriptors = dma
        .coherent_array_zero_with_align::<Adma2Desc32>(ADMA2_DESC_COUNT, ADMA2_DESC_ALIGN)
        .unwrap();
    host.enable_completion_irq();
    host.write_u32(REG_PRESENT_STATE, PRESENT_CMD_INHIBIT | PRESENT_DAT_INHIBIT);
    host.write_u32(REG_ADMA_SYS_ADDR_LOW, ORIGINAL_ADMA_ADDR);

    assert_eq!(
        host.submit_adma2_blocks_mapped(
            &cmd17(0),
            1,
            0x1000_0000,
            &mut descriptors,
            DataDirection::Read,
            Phase::DataRead,
        ),
        Err(Error::Busy)
    );
    assert_eq!(host.read_u32(REG_ADMA_SYS_ADDR_LOW), ORIGINAL_ADMA_ADDR);
    assert_eq!(host.read_u16(REG_COMMAND), 0);
}

#[test]
fn unbound_irq_fails_closed_without_publishing_adma_state() {
    const ORIGINAL_ADMA_ADDR: u32 = 0x1234_5000;
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    let dma = DeviceDma::new_legacy(u32::MAX as u64, &TEST_DMA);
    let mut descriptors = dma
        .coherent_array_zero_with_align::<Adma2Desc32>(ADMA2_DESC_COUNT, ADMA2_DESC_ALIGN)
        .unwrap();
    host.write_u32(REG_PRESENT_STATE, PRESENT_CMD_INHIBIT | PRESENT_DAT_INHIBIT);
    host.write_u32(REG_ADMA_SYS_ADDR_LOW, ORIGINAL_ADMA_ADDR);

    assert_eq!(
        host.submit_adma2_blocks_mapped(
            &cmd17(0),
            1,
            0x1000_0000,
            &mut descriptors,
            DataDirection::Read,
            Phase::DataRead,
        ),
        Err(Error::UnsupportedCommand)
    );
    assert_eq!(host.read_u32(REG_ADMA_SYS_ADDR_LOW), ORIGINAL_ADMA_ADDR);
    assert!(matches!(host.command_state, CommandState::Idle));
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
fn dropping_an_active_adma_request_quarantines_its_descriptor_table() {
    DROP_AUDIT_COHERENT_DEALLOCATIONS.store(0, Ordering::Relaxed);
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.write_u32(REG_CAPABILITIES_LOW, CAPS_LOW_ADMA2_SUPPORTED);
    host.enable_completion_irq();

    let dma = DeviceDma::new_legacy(u32::MAX as u64, &DROP_AUDIT_DMA);
    let buffer = CpuDmaBuffer::new_zero(
        &dma,
        NonZeroUsize::new(BLOCK_SIZE).unwrap(),
        BLOCK_SIZE,
        DmaDirection::FromDevice,
    )
    .unwrap()
    .prepare_for_device();
    let mut slot = BlockRequestSlot::default();
    let request = match host.submit_prepared_read_blocks(0, buffer, &dma, &mut slot) {
        Ok(request) => request,
        Err(error) => panic!("ADMA request setup failed: {}", error.error),
    };

    drop(request);

    assert_eq!(
        DROP_AUDIT_COHERENT_DEALLOCATIONS.load(Ordering::Relaxed),
        0,
        "an unquiesced controller may still fetch the ADMA table"
    );
}

#[test]
fn owned_fifo_read_completes_from_irq_snapshots_and_returns_same_buffer() {
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.enable_completion_irq();
    host.write_u32(REG_BUFFER_DATA_PORT, 0x4433_2211);
    let dma = DeviceDma::new_legacy(u64::MAX, &TEST_DMA);
    let buffer = CpuDmaBuffer::new_zero(
        &dma,
        NonZeroUsize::new(BLOCK_SIZE).unwrap(),
        BLOCK_SIZE,
        DmaDirection::FromDevice,
    )
    .unwrap();
    let original_ptr = buffer.cpu_ptr();
    let mut slot = BlockRequestSlot::default();
    let request = host
        .submit_owned_fifo_data_request(
            &cmd17(0),
            buffer,
            BLOCK_SIZE as u32,
            1,
            DataDirection::Read,
            &mut slot,
        )
        .unwrap();
    let id = request.id();
    let mut request = Some(request);
    let generation = host.irq.state.generation();
    host.irq.state.cache_if_current(
        generation,
        NORMAL_INT_CMD_COMPLETE | NORMAL_INT_BUFFER_READ_READY,
        0,
    );

    assert_eq!(
        host.service_block_request(&mut request, id, &mut slot),
        Ok(BlockPoll::Pending)
    );
    assert!(slot.take_completed_cpu().is_none());
    host.irq
        .state
        .cache_if_current(generation, NORMAL_INT_XFER_COMPLETE, 0);
    assert_eq!(
        host.service_block_request(&mut request, id, &mut slot),
        Ok(BlockPoll::Complete)
    );

    let completed = slot.take_completed_cpu().unwrap();
    assert_eq!(completed.cpu_ptr(), original_ptr);
    assert_eq!(
        &completed.as_slice_cpu()[..4],
        &0x4433_2211u32.to_le_bytes()
    );
}

#[test]
fn owned_fifo_read_consumes_coalesced_ready_and_transfer_complete() {
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.enable_completion_irq();
    host.write_u32(REG_BUFFER_DATA_PORT, 0x4433_2211);
    let dma = DeviceDma::new_legacy(u64::MAX, &TEST_DMA);
    let buffer = CpuDmaBuffer::new_zero(
        &dma,
        NonZeroUsize::new(BLOCK_SIZE).unwrap(),
        BLOCK_SIZE,
        DmaDirection::FromDevice,
    )
    .unwrap();
    let original_ptr = buffer.cpu_ptr();
    let mut slot = BlockRequestSlot::default();
    let request = host
        .submit_owned_fifo_data_request(
            &cmd17(0),
            buffer,
            BLOCK_SIZE as u32,
            1,
            DataDirection::Read,
            &mut slot,
        )
        .unwrap();
    let id = request.id();
    let mut request = Some(request);
    let generation = host.irq.state.generation();
    host.irq.state.cache_if_current(
        generation,
        NORMAL_INT_CMD_COMPLETE | NORMAL_INT_BUFFER_READ_READY | NORMAL_INT_XFER_COMPLETE,
        0,
    );

    assert_eq!(
        host.service_block_request(&mut request, id, &mut slot),
        Ok(BlockPoll::Complete),
        "one acknowledged IRQ snapshot must not require a synthetic second wake"
    );
    assert_eq!(slot.take_completed_cpu().unwrap().cpu_ptr(), original_ptr);
}

#[test]
fn owned_fifo_write_completes_from_irq_snapshots_and_returns_same_buffer() {
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.enable_completion_irq();
    let dma = DeviceDma::new_legacy(u64::MAX, &TEST_DMA);
    let mut buffer = CpuDmaBuffer::new_zero(
        &dma,
        NonZeroUsize::new(BLOCK_SIZE).unwrap(),
        BLOCK_SIZE,
        DmaDirection::ToDevice,
    )
    .unwrap();
    let original_ptr = buffer.cpu_ptr();
    let mut contents = [0u8; BLOCK_SIZE];
    contents[BLOCK_SIZE - 4..].copy_from_slice(&0x8877_6655u32.to_le_bytes());
    buffer.copy_to_device_from_slice(&contents);
    let mut slot = BlockRequestSlot::default();
    let request = host
        .submit_owned_fifo_data_request(
            &cmd24(0),
            buffer,
            BLOCK_SIZE as u32,
            1,
            DataDirection::Write,
            &mut slot,
        )
        .unwrap();
    let id = request.id();
    let mut request = Some(request);
    let generation = host.irq.state.generation();
    host.irq.state.cache_if_current(
        generation,
        NORMAL_INT_CMD_COMPLETE | NORMAL_INT_BUFFER_WRITE_READY,
        0,
    );

    assert_eq!(
        host.service_block_request(&mut request, id, &mut slot),
        Ok(BlockPoll::Pending)
    );
    assert_eq!(host.read_u32(REG_BUFFER_DATA_PORT), 0x8877_6655);
    host.irq
        .state
        .cache_if_current(generation, NORMAL_INT_XFER_COMPLETE, 0);
    assert_eq!(
        host.service_block_request(&mut request, id, &mut slot),
        Ok(BlockPoll::Complete)
    );

    assert_eq!(slot.take_completed_cpu().unwrap().cpu_ptr(), original_ptr);
}

#[test]
fn owned_fifo_write_consumes_coalesced_ready_and_transfer_complete() {
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.enable_completion_irq();
    let dma = DeviceDma::new_legacy(u64::MAX, &TEST_DMA);
    let buffer = CpuDmaBuffer::new_zero(
        &dma,
        NonZeroUsize::new(BLOCK_SIZE).unwrap(),
        BLOCK_SIZE,
        DmaDirection::ToDevice,
    )
    .unwrap();
    let original_ptr = buffer.cpu_ptr();
    let mut slot = BlockRequestSlot::default();
    let request = host
        .submit_owned_fifo_data_request(
            &cmd24(0),
            buffer,
            BLOCK_SIZE as u32,
            1,
            DataDirection::Write,
            &mut slot,
        )
        .unwrap();
    let id = request.id();
    let mut request = Some(request);
    let generation = host.irq.state.generation();
    host.irq.state.cache_if_current(
        generation,
        NORMAL_INT_CMD_COMPLETE | NORMAL_INT_BUFFER_WRITE_READY | NORMAL_INT_XFER_COMPLETE,
        0,
    );

    assert_eq!(
        host.service_block_request(&mut request, id, &mut slot),
        Ok(BlockPoll::Complete),
        "one acknowledged IRQ snapshot must not require a synthetic second wake"
    );
    assert_eq!(slot.take_completed_cpu().unwrap().cpu_ptr(), original_ptr);
}

#[test]
fn owned_fifo_submit_failure_returns_same_buffer_without_starting_request() {
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    let dma = DeviceDma::new_legacy(u64::MAX, &TEST_DMA);
    let buffer = CpuDmaBuffer::new_zero(
        &dma,
        NonZeroUsize::new(BLOCK_SIZE).unwrap(),
        BLOCK_SIZE,
        DmaDirection::FromDevice,
    )
    .unwrap();
    let original_ptr = buffer.cpu_ptr();
    let mut slot = BlockRequestSlot::default();

    let error = match host.submit_owned_fifo_data_request(
        &cmd17(0),
        buffer,
        BLOCK_SIZE as u32,
        1,
        DataDirection::Write,
        &mut slot,
    ) {
        Ok(_) => panic!("direction-mismatched owned request was accepted"),
        Err(error) => error,
    };

    assert_eq!(error.error, Error::InvalidArgument);
    assert_eq!(error.into_buffer().cpu_ptr(), original_ptr);
    assert!(matches!(slot.state(), BlockTransferState::Idle));
}

#[test]
fn owned_fifo_error_returns_buffer_only_after_controller_quiescence() {
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.enable_completion_irq();
    let dma = DeviceDma::new_legacy(u64::MAX, &TEST_DMA);
    let buffer = CpuDmaBuffer::new_zero(
        &dma,
        NonZeroUsize::new(BLOCK_SIZE).unwrap(),
        BLOCK_SIZE,
        DmaDirection::FromDevice,
    )
    .unwrap();
    let original_ptr = buffer.cpu_ptr();
    let mut slot = BlockRequestSlot::default();
    let request = host
        .submit_owned_fifo_data_request(
            &cmd17(0),
            buffer,
            BLOCK_SIZE as u32,
            1,
            DataDirection::Read,
            &mut slot,
        )
        .unwrap();
    let id = request.id();
    let mut request = Some(request);
    let generation = host.irq.state.generation();
    host.irq
        .state
        .cache_if_current(generation, NORMAL_INT_ERROR, ERROR_INT_DATA_TIMEOUT);

    assert!(matches!(
        host.service_block_request(&mut request, id, &mut slot),
        Err(Error::Timeout(_))
    ));
    assert!(request.is_some());
    assert!(slot.take_completed_cpu().is_none());
    assert_eq!(
        host.abort_block_request_response(&mut request, id, &mut slot),
        Err(Error::Busy),
        "request ownership must remain retained before lifecycle quiescence"
    );

    host.recovery_quiesced = true;
    host.abort_block_request_response(&mut request, id, &mut slot)
        .unwrap();
    assert_eq!(slot.take_completed_cpu().unwrap().cpu_ptr(), original_ptr);
}

#[test]
fn block_poll_consumes_data_complete_cached_with_command_complete() {
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    let mut slot = BlockRequestSlot::default();
    let id = slot
        .start(BlockTransferMode::Fifo, BlockTransferDirection::Write)
        .unwrap();
    let buffer = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut request = Some(BlockRequest {
        inner: BlockRequestKind::FifoWrite {
            id,
            buffer,
            owned_cpu: None,
            len: 0,
            block_size: BLOCK_SIZE,
            offset: 0,
            cmd_index: 24,
            phase: Phase::DataWrite,
            stage: BlockRequestStage::Command,
            stop_after_complete: false,
            response: None,
        },
    });
    host.command_state = CommandState::Complete {
        response: Response::Empty,
    };
    host.active_data_cmd = 24;
    host.enable_completion_irq();
    host.irq.state.begin_request();
    let generation = host.irq.state.generation();
    host.irq.state.cache_if_current(
        generation,
        NORMAL_INT_CMD_COMPLETE | NORMAL_INT_XFER_COMPLETE,
        0,
    );

    assert_eq!(
        host.service_block_request(&mut request, id, &mut slot),
        Ok(BlockPoll::Complete)
    );
    assert!(request.is_none());
    assert!(matches!(slot.state(), BlockTransferState::Idle));
}

#[test]
fn adma_completion_consumes_boundary_before_cmd12_handoff() {
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.enable_completion_irq();
    assert!(host.irq.state.begin_request());
    let generation = host.irq.state.generation();
    host.irq.state.cache_if_current(
        generation,
        NORMAL_INT_XFER_COMPLETE | NORMAL_INT_DMA_INTERRUPT,
        0,
    );

    assert_eq!(
        host.poll_data_complete_with_adma(25, Phase::DataWrite),
        Ok(BlockPoll::Complete)
    );
    assert_eq!(host.pending_irq.normal, 0);
    assert_eq!(host.pending_irq.error, 0);
    assert!(host.irq.state.request_handoff_ready());
    assert_eq!(
        host.ensure_command_admissible(&sdmmc_protocol::cmd::CMD12, false),
        Ok(())
    );
    assert_eq!(
        host.submit_command(&sdmmc_protocol::cmd::CMD12),
        Ok(()),
        "the acknowledged ADMA boundary belongs to the completed data epoch"
    );
    assert!(matches!(host.command_state, CommandState::Issued { .. }));
}

#[test]
fn transfer_completion_never_hides_coalesced_adma_error() {
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.enable_completion_irq();
    host.irq.state.begin_request();
    let generation = host.irq.state.generation();
    host.irq.state.cache_if_current(
        generation,
        NORMAL_INT_XFER_COMPLETE | NORMAL_INT_ERROR,
        ERROR_INT_ADMA,
    );

    assert_eq!(
        host.poll_data_complete_with_adma(18, Phase::DataRead),
        Err(Error::Misaligned)
    );
}

#[test]
fn missing_data_irq_remains_pending_for_the_external_watchdog() {
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.enable_completion_irq();
    host.pending_data = Some(crate::host::PendingData {
        direction: DataDirection::Read,
        block_size: BLOCK_SIZE as u32,
        block_count: 1,
        adma_descriptor: None,
    });
    host.submit_command(&sdmmc_protocol::cmd::cmd17(0)).unwrap();

    for _ in 0..128 {
        assert_eq!(
            host.poll_data_complete_with_adma(17, Phase::DataRead),
            Ok(BlockPoll::Pending)
        );
    }
}

#[test]
fn watchdog_never_promotes_unacknowledged_hardware_status_to_completion() {
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.enable_completion_irq();
    host.pending_data = Some(crate::host::PendingData {
        direction: DataDirection::Read,
        block_size: BLOCK_SIZE as u32,
        block_count: 1,
        adma_descriptor: None,
    });
    host.submit_command(&sdmmc_protocol::cmd::cmd17(0)).unwrap();

    // A completion bit that never passed through the IRQ endpoint is not
    // evidence that task context may consume. The external watchdog owns
    // failure and controller recovery.
    host.write_u16(REG_NORMAL_INT_STATUS, NORMAL_INT_XFER_COMPLETE);

    assert_eq!(
        host.poll_data_complete_with_adma(17, Phase::DataRead),
        Ok(BlockPoll::Pending)
    );
    assert_eq!(
        host.read_u16(REG_NORMAL_INT_STATUS),
        NORMAL_INT_XFER_COMPLETE
    );
}

#[test]
fn unbound_fifo_write_does_not_use_present_state_as_irq_evidence() {
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    let mut buffer = [0x5au8; BLOCK_SIZE];
    buffer[BLOCK_SIZE - 4..].copy_from_slice(&0x1122_3344u32.to_le_bytes());
    let ptr = NonNull::new(buffer.as_mut_ptr()).unwrap();
    let mut offset = 0;
    host.write_u32(REG_PRESENT_STATE, PRESENT_BUFFER_WRITE_ENABLE);

    assert_eq!(
        poll_fifo_write_step(
            &mut host,
            ptr,
            buffer.len(),
            BLOCK_SIZE,
            &mut offset,
            24,
            Phase::DataWrite,
        ),
        Ok(BlockPoll::Pending)
    );

    assert_eq!(offset, 0);
}

#[test]
fn runtime_fifo_write_requires_an_irq_snapshot_even_when_present_state_is_ready() {
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    let mut buffer = [0x5au8; BLOCK_SIZE];
    let ptr = NonNull::new(buffer.as_mut_ptr()).unwrap();
    let mut offset = 0;
    host.enable_completion_irq();
    host.irq.state.begin_request();
    host.write_u32(REG_PRESENT_STATE, PRESENT_BUFFER_WRITE_ENABLE);

    assert_eq!(
        poll_fifo_write_step(
            &mut host,
            ptr,
            buffer.len(),
            BLOCK_SIZE,
            &mut offset,
            24,
            Phase::DataWrite,
        ),
        Ok(BlockPoll::Pending)
    );
    assert_eq!(
        offset, 0,
        "runtime FIFO progress must require IRQ endpoint evidence"
    );
}

#[test]
fn masked_runtime_fifo_progress_still_requires_the_irq_endpoint() {
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    let mut buffer = [0x5au8; BLOCK_SIZE];
    let ptr = NonNull::new(buffer.as_mut_ptr()).unwrap();
    let mut offset = 0;
    let (mut irq, _control) = host.take_irq_source().unwrap().into_parts();
    host.enable_completion_irq();
    host.irq.state.begin_request();
    host.disable_completion_irq();
    host.write_u32(REG_PRESENT_STATE, PRESENT_BUFFER_WRITE_ENABLE);
    host.write_u16(REG_NORMAL_INT_STATUS, NORMAL_INT_BUFFER_WRITE_READY);

    assert_eq!(
        poll_fifo_write_step(
            &mut host,
            ptr,
            buffer.len(),
            BLOCK_SIZE,
            &mut offset,
            24,
            Phase::DataWrite,
        ),
        Ok(BlockPoll::Pending)
    );
    assert_eq!(offset, 0);

    assert!(matches!(
        irq.capture(),
        IrqCapture::Captured {
            event,
            masked: None,
        } if event == crate::Event::from_status(NORMAL_INT_BUFFER_WRITE_READY, 0)
    ));
    assert_eq!(
        poll_fifo_write_step(
            &mut host,
            ptr,
            buffer.len(),
            BLOCK_SIZE,
            &mut offset,
            24,
            Phase::DataWrite,
        ),
        Ok(BlockPoll::Pending)
    );
    assert_eq!(offset, BLOCK_SIZE);
}

#[test]
fn runtime_fifo_error_is_reported_without_task_context_reset() {
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    let mut buffer = [0u8; BLOCK_SIZE];
    let ptr = NonNull::new(buffer.as_mut_ptr()).unwrap();
    let mut offset = 0;
    host.enable_completion_irq();
    host.irq.state.begin_request();
    let generation = host.irq.state.generation();
    host.irq
        .state
        .cache_if_current(generation, NORMAL_INT_ERROR, ERROR_INT_DATA_TIMEOUT);

    assert!(matches!(
        poll_fifo_write_step(
            &mut host,
            ptr,
            buffer.len(),
            BLOCK_SIZE,
            &mut offset,
            24,
            Phase::DataWrite,
        ),
        Err(Error::Timeout(_))
    ));
    assert_eq!(
        host.read_u8(REG_SOFTWARE_RESET),
        0,
        "runtime errors must enter the controller lifecycle instead of resetting inline"
    );
}

#[test]
fn unbound_fifo_read_does_not_use_present_state_as_irq_evidence() {
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    let mut buffer = [0u8; BLOCK_SIZE];
    let ptr = NonNull::new(buffer.as_mut_ptr()).unwrap();
    let mut offset = 0;
    host.write_u32(REG_PRESENT_STATE, PRESENT_BUFFER_READ_ENABLE);
    host.write_u32(REG_BUFFER_DATA_PORT, 0xaabb_ccdd);

    assert_eq!(
        poll_fifo_read_step(
            &mut host,
            ptr,
            4,
            BLOCK_SIZE,
            &mut offset,
            17,
            Phase::DataRead,
        ),
        Ok(BlockPoll::Pending)
    );

    assert_eq!(offset, 0);
    assert_eq!(&buffer[..4], &[0; 4]);
}

#[test]
fn runtime_fifo_read_requires_an_irq_snapshot_even_when_present_state_is_ready() {
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    let mut buffer = [0u8; BLOCK_SIZE];
    let ptr = NonNull::new(buffer.as_mut_ptr()).unwrap();
    let mut offset = 0;
    host.enable_completion_irq();
    host.irq.state.begin_request();
    host.write_u32(REG_PRESENT_STATE, PRESENT_BUFFER_READ_ENABLE);
    host.write_u32(REG_BUFFER_DATA_PORT, 0xaabb_ccdd);

    assert_eq!(
        poll_fifo_read_step(
            &mut host,
            ptr,
            4,
            BLOCK_SIZE,
            &mut offset,
            17,
            Phase::DataRead,
        ),
        Ok(BlockPoll::Pending)
    );
    assert_eq!(
        offset, 0,
        "runtime FIFO progress must require IRQ endpoint evidence"
    );
    assert_eq!(&buffer[..4], &[0; 4]);
}

#[test]
fn unbound_fifo_completion_does_not_use_data_line_state() {
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    let mut buffer = [0u8; BLOCK_SIZE];
    let ptr = NonNull::new(buffer.as_mut_ptr()).unwrap();
    let mut offset = BLOCK_SIZE;
    host.write_u32(REG_PRESENT_STATE, 0);

    assert_eq!(
        poll_fifo_read_step(
            &mut host,
            ptr,
            BLOCK_SIZE,
            BLOCK_SIZE,
            &mut offset,
            17,
            Phase::DataRead,
        ),
        Ok(BlockPoll::Pending)
    );
}

#[test]
fn runtime_fifo_completion_requires_transfer_complete_irq() {
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    let mut buffer = [0u8; BLOCK_SIZE];
    let ptr = NonNull::new(buffer.as_mut_ptr()).unwrap();
    let mut offset = BLOCK_SIZE;
    host.enable_completion_irq();
    host.irq.state.begin_request();
    host.write_u32(REG_PRESENT_STATE, 0);

    assert_eq!(
        poll_fifo_read_step(
            &mut host,
            ptr,
            BLOCK_SIZE,
            BLOCK_SIZE,
            &mut offset,
            17,
            Phase::DataRead,
        ),
        Ok(BlockPoll::Pending)
    );
}

#[test]
fn unbound_fifo_write_waits_without_irq_evidence() {
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    let mut buffer = [0u8; BLOCK_SIZE];
    let ptr = NonNull::new(buffer.as_mut_ptr()).unwrap();
    let mut offset = BLOCK_SIZE;
    host.write_u32(REG_PRESENT_STATE, PRESENT_DAT_INHIBIT);

    assert_eq!(
        poll_fifo_write_step(
            &mut host,
            ptr,
            BLOCK_SIZE,
            BLOCK_SIZE,
            &mut offset,
            24,
            Phase::DataWrite,
        ),
        Ok(BlockPoll::Pending)
    );
}

#[test]
fn unbound_fifo_write_does_not_use_dat0_as_completion() {
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    let mut buffer = [0u8; BLOCK_SIZE];
    let ptr = NonNull::new(buffer.as_mut_ptr()).unwrap();
    let mut offset = BLOCK_SIZE;
    host.write_u32(
        REG_PRESENT_STATE,
        PRESENT_DAT_INHIBIT | PRESENT_DAT0_LINE_SIGNAL_LEVEL,
    );

    assert_eq!(
        poll_fifo_write_step(
            &mut host,
            ptr,
            BLOCK_SIZE,
            BLOCK_SIZE,
            &mut offset,
            24,
            Phase::DataWrite,
        ),
        Ok(BlockPoll::Pending)
    );
}
