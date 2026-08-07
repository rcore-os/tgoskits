use super::*;

#[test]
fn first_descriptor_sets_owned_chained_first_read_buffer() {
    let desc = IdmacDesc::chained(0x1234_5000, 512, 0x2000, true, false);

    assert_eq!(desc.des0, DESC_OWN | DESC_CH | DESC_FS | DESC_DIC);
    assert_eq!(desc.des1, 512);
    assert_eq!(desc.des2, 0x1234_5000);
    assert_eq!(desc.des3, 0x2000);
}

#[test]
fn last_descriptor_sets_last_and_terminates_chain() {
    let desc = IdmacDesc::chained(0x1234_5200, 512, 0, false, true);

    assert_eq!(desc.des0, DESC_OWN | DESC_LD);
    assert_eq!(desc.des1, 512);
    assert_eq!(desc.des2, 0x1234_5200);
    assert_eq!(desc.des3, 0);
}

#[test]
fn single_descriptor_requests_completion_interrupt() {
    let desc = IdmacDesc::chained(0x1234_5000, 512, 0, true, true);

    assert_eq!(desc.des0, DESC_OWN | DESC_FS | DESC_LD);
    assert_eq!(desc.des1, 512);
    assert_eq!(desc.des2, 0x1234_5000);
    assert_eq!(desc.des3, 0);
}

#[test]
fn idmac_descriptor_payload_is_limited_to_four_kib() {
    assert_eq!(IDMAC_DESC_MAX_BYTES, 4096);
    let desc = IdmacDesc::chained(0x1234_5000, IDMAC_DESC_MAX_BYTES as u32, 0, true, true);
    assert_eq!(desc.des1 as usize, IDMAC_DESC_MAX_BYTES);
}

#[test]
fn dma_read_plan_rejects_non_block_sized_buffers() {
    let size = NonZeroUsize::new(513).unwrap();

    assert_eq!(dma_read_block_count(size), Err(Error::Misaligned));
}

#[test]
fn dma_read_plan_reports_block_count() {
    let size = NonZeroUsize::new(1024).unwrap();

    assert_eq!(dma_read_block_count(size), Ok(2));
}

#[test]
fn dma_write_plan_rejects_non_block_sized_buffers() {
    let size = NonZeroUsize::new(513).unwrap();

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
fn stopping_idmac_preserves_controller_completion_irqs() {
    let mut mmio = [0u32; 256];
    let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
    let mut host = unsafe { DwMmc::new(base) };
    host.enable_completion_irq();
    assert!(host.regs.ctrl().read().int_enable());

    host.disable_idmac();

    assert!(
        host.regs.ctrl().read().int_enable(),
        "quiescing DMA must not mask a following command-only completion"
    );
}

#[test]
fn idmac_ring_splits_4608_bytes_into_4096_and_512() {
    let mut descriptors = [IdmacDesc::default(); 4];

    let count = prepare_idmac_descriptors(&mut descriptors, 0x1000, 0x4000, 4608).unwrap();

    assert_eq!(count, 2);
    assert_eq!(descriptors[0].des1, 4096);
    assert_eq!(descriptors[0].des2, 0x4000);
    assert_eq!(descriptors[0].des3, 0x1000 + IDMAC_DESC_SIZE as u32);
    assert_eq!(descriptors[0].des0, DESC_OWN | DESC_CH | DESC_FS | DESC_DIC);
    assert_eq!(descriptors[1].des1, 512);
    assert_eq!(descriptors[1].des2, 0x5000);
    assert_eq!(descriptors[1].des3, 0);
    assert_eq!(descriptors[1].des0, DESC_OWN | DESC_LD);
}

#[test]
fn idmac_ring_does_not_rewrite_descriptors_after_the_terminal_entry() {
    let sentinel = IdmacDesc {
        des0: 0x11,
        des1: 0x22,
        des2: 0x33,
        des3: 0x44,
    };
    let mut descriptors = [IdmacDesc::default(); 4];
    descriptors[3] = sentinel;

    let count = prepare_idmac_descriptors(&mut descriptors, 0x1000, 0x4000, 512).unwrap();

    assert_eq!(count, 1);
    assert_eq!(descriptors[3], sentinel);
}

#[test]
fn idmac_ring_does_not_cross_a_four_kib_dma_boundary() {
    let mut descriptors = [IdmacDesc::default(); 4];

    let count = prepare_idmac_descriptors(&mut descriptors, 0x1000, 0x4f00, 4096).unwrap();

    assert_eq!(count, 2);
    assert_eq!(descriptors[0].des1, 256);
    assert_eq!(descriptors[0].des2, 0x4f00);
    assert_eq!(descriptors[0].des3, 0x1000 + IDMAC_DESC_SIZE as u32);
    assert_eq!(descriptors[0].des0, DESC_OWN | DESC_CH | DESC_FS | DESC_DIC);
    assert_eq!(descriptors[1].des1, 3840);
    assert_eq!(descriptors[1].des2, 0x5000);
    assert_eq!(descriptors[1].des3, 0);
    assert_eq!(descriptors[1].des0, DESC_OWN | DESC_LD);
}

#[test]
fn idmac_ring_rejects_more_payload_than_descriptor_capacity() {
    let mut descriptors = [IdmacDesc::default(); 1];

    assert_eq!(
        prepare_idmac_descriptors(&mut descriptors, 0x1000, 0x4000, IDMAC_DESC_MAX_BYTES + 1,),
        Err(Error::InvalidArgument)
    );
}

#[test]
fn task_side_does_not_consume_unacknowledged_raw_irq_status() {
    let mut mmio = [0u32; 256];
    let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
    let mut host = unsafe { DwMmc::new(base) };
    const RINTSTS_WORD: usize = 17;
    let raw = crate::regs::RIntSts::new()
        .with_data_transfer_over(true)
        .into_bits();
    host.irq.state.begin_request();
    unsafe {
        mmio.as_mut_ptr().add(RINTSTS_WORD).write_volatile(raw);
    }

    assert_eq!(host.take_data_irq_status(), 0);
    assert_eq!(
        unsafe { mmio.as_ptr().add(RINTSTS_WORD).read_volatile() },
        raw
    );
}

#[test]
fn controller_and_idmac_completion_must_both_arrive() {
    let mut mmio = [0u32; 256];
    let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
    let mut host = unsafe { DwMmc::new(base) };
    host.irq.state.begin_request();
    let generation = host.irq.state.generation();

    host.irq
        .state
        .cache_if_current(generation, crate::DWMMC_INT_DATA_TRANSFER_OVER);
    assert_eq!(
        host.consume_dma_completion(18, Phase::DataRead).unwrap(),
        BlockProgress::Pending
    );

    host.irq.state.cache_if_current(generation, 1 << 30);
    assert_eq!(
        host.consume_dma_completion(18, Phase::DataRead).unwrap(),
        BlockProgress::Complete
    );
}
