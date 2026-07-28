use core::ptr::NonNull;

use sdmmc_protocol::response::Response;

use super::{
    fifo::{poll_fifo_read_step, poll_fifo_write_step},
    *,
};

#[repr(align(4))]
struct FakeRegs([u8; 0x100]);

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
    host.enable_completion_irq();
    host.irq.state.begin_request();
    let generation = host.irq.state.generation();
    host.irq.state.cache_if_current(
        generation,
        NORMAL_INT_CMD_COMPLETE | NORMAL_INT_XFER_COMPLETE,
        0,
    );

    assert!(matches!(
        host.progress_block_request(&mut request, id, &mut slot),
        Ok(DataCommandPoll::Complete(Response::Empty))
    ));
    assert!(request.is_none());
    assert!(matches!(slot.state, BlockTransferState::Idle));
}

#[test]
fn fifo_write_step_accepts_present_state_ready_without_irq_status() {
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

    assert_eq!(offset, BLOCK_SIZE);
    assert_eq!(host.read_u32(REG_BUFFER_DATA_PORT), 0x1122_3344);
}

#[test]
fn fifo_read_step_accepts_present_state_ready_without_irq_status() {
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

    assert_eq!(offset, 4);
    assert_eq!(&buffer[..4], &0xaabb_ccddu32.to_le_bytes());
}

#[test]
fn fifo_data_complete_accepts_dat_inhibit_clear_without_irq_status() {
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
        Ok(BlockPoll::Complete)
    );
}

#[test]
fn fifo_write_complete_waits_while_dat0_busy_without_xfer_irq() {
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
fn fifo_write_complete_accepts_dat0_ready_without_xfer_irq_or_write_ready() {
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
        Ok(BlockPoll::Complete)
    );
}
