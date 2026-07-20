#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idmac_descriptor_builder_marks_single_descriptor_chain() {
        let descriptors = build_idmac_descriptors(0x1_2345_6000, 0x8000_0000, 4096, 4096).unwrap();

        assert_eq!(descriptors.len(), 1);
        assert_eq!(
            descriptors[0].attribute,
            IDMAC_DESC_OWN
                | IDMAC_DESC_CHAIN
                | IDMAC_DESC_FIRST
                | IDMAC_DESC_LAST
                | IDMAC_DESC_END_RING
        );
        assert_eq!(descriptors[0].len, 4096);
        assert_eq!(descriptors[0].addr_lo, 0x2345_6000);
        assert_eq!(descriptors[0].addr_hi, 0x0000_0001);
        assert_eq!(descriptors[0].desc_lo, 0);
        assert_eq!(descriptors[0].desc_hi, 0);
    }

    #[test]
    fn idmac_descriptor_builder_chains_multiple_descriptors() {
        let descriptors =
            build_idmac_descriptors(0x4000_0000, 0x8000_0000, 0x3000, 0x1000).unwrap();

        assert_eq!(descriptors.len(), 3);
        assert_eq!(
            descriptors[0].attribute,
            IDMAC_DESC_OWN | IDMAC_DESC_CHAIN | IDMAC_DESC_FIRST
        );
        assert_eq!(
            descriptors[0].desc_lo,
            0x8000_0000 + core::mem::size_of::<IdmacDesc>() as u32
        );
        assert_eq!(descriptors[1].attribute, IDMAC_DESC_OWN | IDMAC_DESC_CHAIN);
        assert_eq!(
            descriptors[2].attribute,
            IDMAC_DESC_OWN | IDMAC_DESC_CHAIN | IDMAC_DESC_LAST | IDMAC_DESC_END_RING
        );
        assert_eq!(descriptors[2].desc_lo, 0);
    }

    #[test]
    fn idmac_interrupt_mask_enables_terminal_status_bits() {
        assert_ne!(IDSTS_INT_ENABLE_MASK & IDSTS_RECEIVE, 0);
        assert_ne!(IDSTS_INT_ENABLE_MASK & IDSTS_TRANSMIT, 0);
        assert_ne!(IDSTS_INT_ENABLE_MASK & IDSTS_NORMAL_SUMMARY, 0);
    }

    use core::{
        num::{NonZeroU16, NonZeroU32},
        ptr::NonNull,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use ::alloc::{alloc, boxed::Box};
    use sdmmc_protocol::block::BlockPoll;

    use crate::regs::{RIntSts, Status};

    #[repr(align(512))]
    struct AlignedBlock([u8; BLOCK_SIZE]);

    struct NoopDmaBuffer;

    impl NoopDmaBuffer {
        fn progress() -> DmaProgress {
            let dma = DeviceDma::new_legacy(u64::MAX, &TEST_DMA);
            let descriptors = dma
                .coherent_array_zero_with_align::<IdmacDesc>(1, IDMAC_DESC_ALIGN)
                .unwrap();
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
                descriptors,
                buffer,
                desc_count: 1,
                complete: false,
                idmac_done: false,
                data_done: false,
            }
        }

        fn owned_progress() -> DmaProgress {
            let dma = DeviceDma::new_legacy(u64::MAX, &TEST_DMA);
            let descriptors = dma
                .coherent_array_zero_with_align::<IdmacDesc>(1, IDMAC_DESC_ALIGN)
                .unwrap();
            let buffer = CpuDmaBuffer::new_zero(
                &dma,
                NonZeroUsize::new(BLOCK_SIZE).unwrap(),
                BLOCK_SIZE,
                DmaDirection::FromDevice,
            )
            .unwrap()
            .prepare_for_device();
            let buffer = unsafe { buffer.into_in_flight() };
            DmaProgress {
                descriptors,
                buffer: DmaRequestBuffer::Owned(buffer),
                desc_count: 1,
                complete: false,
                idmac_done: false,
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
            Some(unsafe { dma_api::DmaAllocHandle::new(ptr, (ptr.as_ptr() as u64).into(), layout) })
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
            Some(unsafe { dma_api::DmaAllocHandle::new(ptr, (ptr.as_ptr() as u64).into(), layout) })
        }

        unsafe fn dealloc_coherent(&self, handle: dma_api::DmaAllocHandle) {
            unsafe { alloc::dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
        }

        unsafe fn map_streaming(
            &self,
            constraints: dma_api::DmaConstraints,
            addr: NonNull<u8>,
            size: NonZeroUsize,
            _direction: DmaDirection,
        ) -> Result<dma_api::DmaMapHandle, dma_api::DmaError> {
            let layout =
                core::alloc::Layout::from_size_align(size.get(), constraints.align.max(1))?;
            Ok(unsafe {
                dma_api::DmaMapHandle::new(addr, (addr.as_ptr() as u64).into(), layout, None)
            })
        }

        unsafe fn unmap_streaming(&self, _handle: dma_api::DmaMapHandle) {}

        fn flush(&self, _addr: NonNull<u8>, _size: usize) {}
        fn invalidate(&self, _addr: NonNull<u8>, _size: usize) {}
        fn flush_invalidate(&self, _addr: NonNull<u8>, _size: usize) {}
        fn page_size(&self) -> usize {
            4096
        }
    }

    include!("test_support.rs");
    include!("contract_tests.rs");

    const RINTSTS_WORD: usize = 17;
    const STATUS_WORD: usize = 18;
    const CTRL_WORD: usize = 0;
    const BMOD_WORD: usize = 32;
    const PLDMND_WORD: usize = 33;
    const DBADDRL_WORD: usize = 34;
    const IDSTS_WORD: usize = 36;
    const FIFO_WORD: usize = crate::host::DEFAULT_FIFO_OFFSET / core::mem::size_of::<u32>();

    fn host_from_words(words: &mut [u32; 256]) -> PhytiumMci {
        let base = NonNull::new(words.as_mut_ptr().cast()).unwrap();
        unsafe { PhytiumMci::new(base) }
    }

    #[test]
    fn idmac_start_preserves_bus_mode_and_enables_fixed_burst() {
        let mut mmio = [0u32; 256];
        mmio[BMOD_WORD] = 0x200;
        let host = host_from_words(&mut mmio);

        host.program_idmac_registers(0x1_8000_0000);

        assert_eq!(
            mmio[BMOD_WORD],
            0x200 | BMOD_FIXED_BURST | BMOD_IDMAC_ENABLE
        );
        let ctrl = crate::regs::Ctrl::from_bits(mmio[CTRL_WORD]);
        assert!(ctrl.dma_enable());
        assert!(ctrl.use_internal_dmac());
        assert!(!ctrl.int_enable());
        assert_eq!(mmio[PLDMND_WORD], 1);
        assert_eq!(mmio[DBADDRL_WORD], 0x8000_0000);
        assert_eq!(mmio[DBADDRL_WORD + 1], 1);
    }

    #[test]
    fn runtime_data_setup_does_not_ack_raw_irq_status() {
        let mut mmio = [0u32; 256];
        let mut host = host_from_words(&mut mmio);
        let pending = crate::MCI_INT_DATA_TRANSFER_OVER;
        let pending_idmac = IDSTS_RECEIVE;
        mmio[RINTSTS_WORD] = pending;
        mmio[IDSTS_WORD] = pending_idmac;
        host.enable_completion_irq();

        host.prepare_data_irq_for_transfer();

        assert_eq!(mmio[RINTSTS_WORD], pending);
        assert_eq!(mmio[IDSTS_WORD], pending_idmac);
    }

    #[test]
    fn irq_enabled_task_service_cannot_consume_raw_status_before_top_half() {
        let mut mmio = [0u32; 256];
        let mut host = host_from_words(&mut mmio);
        let raw_status = crate::MCI_INT_DATA_TRANSFER_OVER;
        let raw_idmac = IDSTS_RECEIVE;
        host.enable_completion_irq();
        host.irq.state.begin_request();
        unsafe {
            mmio.as_mut_ptr()
                .add(RINTSTS_WORD)
                .write_volatile(raw_status);
            mmio.as_mut_ptr()
                .add(IDSTS_WORD)
                .write_volatile(raw_idmac);
        }

        assert_eq!(
            host.take_data_irq_status(17, Phase::DataRead)
                .unwrap()
                .into_bits(),
            0
        );
        assert_eq!(host.take_idmac_status(), 0);
        assert_eq!(host.irq.state.pending_status(), 0);
        assert_eq!(host.irq.state.pending_idmac_status(), 0);
        assert_eq!(unsafe {
            mmio.as_ptr().add(RINTSTS_WORD).read_volatile()
        }, raw_status);
        assert_eq!(unsafe { mmio.as_ptr().add(IDSTS_WORD).read_volatile() }, raw_idmac);
    }

    #[test]
    fn irq_runtime_submit_does_not_reset_fifo_or_dma_per_request() {
        let mut mmio = [0u32; 256];
        let mut host = host_from_words(&mut mmio);
        host.enable_completion_irq();

        host.start_idmac_transfer(&cmd17(0), BLOCK_SIZE as u32, 1, 0x1000)
            .unwrap();

        let control = crate::regs::Ctrl::from_bits(mmio[CTRL_WORD]);
        assert!(!control.controller_reset());
        assert!(!control.fifo_reset());
        assert!(!control.dma_reset());
        assert!(control.dma_enable());
        assert!(control.use_internal_dmac());
    }

    #[test]
    fn busy_runtime_data_command_is_rejected_before_idmac_is_armed() {
        let mut mmio = [0u32; 256];
        let mut host = host_from_words(&mut mmio);
        unsafe {
            mmio.as_mut_ptr()
                .add(STATUS_WORD)
                .write_volatile(Status::new().with_data_busy(true).into_bits());
        }
        host.enable_completion_irq();

        assert_eq!(
            host.start_idmac_transfer(&cmd17(0), BLOCK_SIZE as u32, 1, 0x1000),
            Err(Error::Busy)
        );
        let control = crate::regs::Ctrl::from_bits(mmio[CTRL_WORD]);
        assert!(!control.dma_enable());
        assert!(!control.use_internal_dmac());
    }

    #[test]
    fn rejected_dma_admission_releases_prepared_backing_without_quarantine() {
        let mut mmio = [0u32; 256];
        let mut host = host_from_words(&mut mmio);
        unsafe {
            mmio.as_mut_ptr()
                .add(STATUS_WORD)
                .write_volatile(Status::new().with_data_busy(true).into_bits());
        }
        host.enable_completion_irq();
        let operations = Box::leak(Box::new(CountingDma::new()));
        let dma = DeviceDma::new_legacy(u64::MAX, operations);
        let mut buffer = AlignedBlock([0; BLOCK_SIZE]);
        let mut slot = BlockRequestSlot::default();

        assert!(matches!(
            host.submit_read_blocks(
                0,
                NonNull::new(buffer.0.as_mut_ptr()).unwrap(),
                NonZeroUsize::new(BLOCK_SIZE).unwrap(),
                Some(&dma),
                BlockTransferMode::Dma,
                &mut slot,
            ),
            Err(Error::Busy)
        ));
        assert_eq!(
            operations.allocations.load(Ordering::Relaxed),
            operations.deallocations.load(Ordering::Relaxed),
            "an unaccepted request must not leak its never-owned DMA backing"
        );
    }

    #[test]
    fn idmac_read_waits_when_data_done_arrives_without_idmac_done() {
        let mut mmio = [0u32; 256];
        let mut host = host_from_words(&mut mmio);
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

        host.irq.state.begin_request();
        let generation = host.irq.state.generation();
        host.irq.state.cache_if_current(
            generation,
            RIntSts::new().with_data_transfer_over(true).into_bits(),
            0,
        );

        assert_eq!(
            host.service_dma_data_event(&mut request, 17, Phase::DataRead)
                .unwrap(),
            BlockPoll::Pending
        );
    }

    #[test]
    fn idmac_read_completes_when_idmac_and_data_done_arrive_separately() {
        let mut mmio = [0u32; 256];
        let mut host = host_from_words(&mut mmio);
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

        host.irq.state.begin_request();
        let generation = host.irq.state.generation();
        host.irq
            .state
            .cache_if_current(generation, 0, IDSTS_RECEIVE);
        assert_eq!(
            host.service_dma_data_event(&mut request, 17, Phase::DataRead)
                .unwrap(),
            BlockPoll::Pending
        );

        host.irq.state.cache_if_current(
            generation,
            RIntSts::new().with_data_transfer_over(true).into_bits(),
            0,
        );
        assert_eq!(
            host.service_dma_data_event(&mut request, 17, Phase::DataRead)
                .unwrap(),
            BlockPoll::Complete
        );
    }

    #[test]
    fn idmac_read_completes_when_data_and_idmac_done_arrive_in_reverse_order() {
        let mut mmio = [0u32; 256];
        let mut host = host_from_words(&mut mmio);
        let mut request = Some(BlockRequest {
            inner: BlockRequestKind::DmaRead {
                id: RequestId::new(4),
                progress: NoopDmaBuffer::progress(),
                cmd_index: 17,
                phase: Phase::DataRead,
                stage: BlockRequestStage::Data,
                stop_after_complete: false,
                response: Some(Response::Empty),
            },
        });

        host.irq.state.begin_request();
        let generation = host.irq.state.generation();
        host.irq.state.cache_if_current(
            generation,
            RIntSts::new().with_data_transfer_over(true).into_bits(),
            0,
        );
        assert_eq!(
            host.service_dma_data_event(&mut request, 17, Phase::DataRead)
                .unwrap(),
            BlockPoll::Pending
        );

        host.irq
            .state
            .cache_if_current(generation, 0, IDSTS_RECEIVE);
        assert_eq!(
            host.service_dma_data_event(&mut request, 17, Phase::DataRead)
                .unwrap(),
            BlockPoll::Complete
        );
    }

    #[test]
    fn late_idmac_error_wins_over_an_earlier_controller_completion() {
        let mut mmio = [0u32; 256];
        let mut host = host_from_words(&mut mmio);
        let mut request = Some(BlockRequest {
            inner: BlockRequestKind::DmaRead {
                id: RequestId::new(5),
                progress: NoopDmaBuffer::progress(),
                cmd_index: 17,
                phase: Phase::DataRead,
                stage: BlockRequestStage::Data,
                stop_after_complete: false,
                response: Some(Response::Empty),
            },
        });

        host.irq.state.begin_request();
        let generation = host.irq.state.generation();
        host.irq.state.cache_if_current(
            generation,
            RIntSts::new().with_data_transfer_over(true).into_bits(),
            0,
        );
        assert_eq!(
            host.service_dma_data_event(&mut request, 17, Phase::DataRead),
            Ok(BlockPoll::Pending)
        );

        host.irq.state.cache_if_current(
            generation,
            0,
            crate::MCI_IDSTS_ERROR_MASK | IDSTS_RECEIVE,
        );
        assert!(matches!(
            host.service_dma_data_event(&mut request, 17, Phase::DataRead),
            Err(Error::BusError(_))
        ));
        assert!(request.is_some(), "recovery must retain DMA ownership");
        host.recovery_quiesced = true;
        drop(host.finish_block_request(request.take().unwrap()));
    }

    #[test]
    fn irq_service_error_retains_request_until_controller_quiescence() {
        let mut mmio = [0u32; 256];
        let mut host = host_from_words(&mut mmio);
        let mut slot = BlockRequestSlot::default();
        let id = slot
            .start(BlockTransferMode::Dma, BlockTransferDirection::Read)
            .unwrap();
        let mut request = Some(BlockRequest {
            inner: BlockRequestKind::DmaRead {
                id,
                progress: NoopDmaBuffer::progress(),
                cmd_index: 17,
                phase: Phase::DataRead,
                stage: BlockRequestStage::Data,
                stop_after_complete: false,
                response: Some(Response::Empty),
            },
        });
        host.enable_completion_irq();
        host.irq.state.begin_request();
        let generation = host.irq.state.generation();
        host.irq
            .state
            .cache_if_current(generation, crate::MCI_INT_DATA_CRC_ERROR, 0);

        assert!(matches!(
            host.service_dma_event(&mut request, id, &mut slot),
            Err(Error::Crc(_))
        ));
        assert!(
            request.is_some(),
            "IRQ service must leave DMA ownership with recovery"
        );
        assert_eq!(slot.state().id(), Some(id));

        host.recovery_quiesced = true;
        host.reclaim_block_request_after_quiesce(&mut request, id, &mut slot)
            .unwrap();
    }

    #[test]
    fn proof_gated_reclaim_requires_quiescence_and_returns_dma_ownership() {
        let mut mmio = [0u32; 256];
        let mut host = host_from_words(&mut mmio);
        let mut slot = BlockRequestSlot::default();
        let id = slot
            .start(BlockTransferMode::Dma, BlockTransferDirection::Read)
            .unwrap();
        let mut request = Some(BlockRequest {
            inner: BlockRequestKind::DmaRead {
                id,
                progress: NoopDmaBuffer::owned_progress(),
                cmd_index: 17,
                phase: Phase::DataRead,
                stage: BlockRequestStage::Data,
                stop_after_complete: false,
                response: Some(Response::Empty),
            },
        });

        assert_eq!(
            host.reclaim_block_request_after_quiesce(&mut request, id, &mut slot),
            Err(Error::Busy)
        );
        assert!(request.is_some());
        assert_eq!(slot.state().id(), Some(id));

        host.recovery_quiesced = true;
        host.reclaim_block_request_after_quiesce(&mut request, id, &mut slot)
            .unwrap();

        assert!(request.is_none());
        assert!(slot.take_completed_dma().is_some());
        assert!(slot.take_completed_dma().is_none());
    }

    #[test]
    fn request_slot_returns_completed_owned_dma_once() {
        let dma = DeviceDma::new_legacy(u64::MAX, &TEST_DMA);
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

    #[test]
    fn fifo_read_completes_when_dto_arrives_before_fifo_is_drained() {
        let mut mmio = [0u32; 256];
        let mut host = host_from_words(&mut mmio);
        let mut buffer = [0u8; 512];
        let mut request = Some(BlockRequest {
            inner: BlockRequestKind::FifoRead {
                id: RequestId::new(1),
                buffer: NonNull::new(buffer.as_mut_ptr()).unwrap(),
                len: buffer.len(),
                block_size: BLOCK_SIZE,
                progress: FifoProgress::default(),
                cmd_index: 17,
                phase: Phase::DataRead,
                stage: BlockRequestStage::Data,
                stop_after_complete: false,
                response: None,
            },
        });

        for index in 0..128 {
            mmio[FIFO_WORD + index] = index as u32;
        }

        host.irq.state.begin_request();
        let generation = host.irq.state.generation();
        host.irq.state.cache_if_current(
            generation,
            RIntSts::new().with_data_transfer_over(true).into_bits(),
            0,
        );
        unsafe {
            mmio.as_mut_ptr()
                .add(STATUS_WORD)
                .write_volatile(Status::new().with_fifo_count(64).into_bits());
        }
        assert_eq!(
            host.service_fifo_data_event(&mut request, 17, Phase::DataRead),
            Ok(BlockPoll::Pending)
        );

        unsafe {
            mmio.as_mut_ptr()
                .add(STATUS_WORD)
                .write_volatile(Status::new().with_fifo_count(64).into_bits());
        }
        assert_eq!(
            host.service_fifo_data_event(&mut request, 17, Phase::DataRead),
            Ok(BlockPoll::Pending)
        );

        host.irq.state.cache_if_current(
            generation,
            RIntSts::new().with_receive_fifo_data_request(true).into_bits(),
            0,
        );
        assert_eq!(
            host.service_fifo_data_event(&mut request, 17, Phase::DataRead),
            Ok(BlockPoll::Complete)
        );
    }

    #[test]
    fn one_irq_service_advances_at_most_sixty_four_fifo_words() {
        let mut mmio = [0u32; 256];
        let mut host = host_from_words(&mut mmio);
        let mut buffer = [0u8; BLOCK_SIZE];
        let mut slot = BlockRequestSlot::default();
        let id = slot
            .start(BlockTransferMode::Fifo, BlockTransferDirection::Read)
            .unwrap();
        let cmd = cmd17(0);
        let mut request = Some(BlockRequest {
            inner: BlockRequestKind::FifoRead {
                id,
                buffer: NonNull::new(buffer.as_mut_ptr()).unwrap(),
                len: buffer.len(),
                block_size: BLOCK_SIZE,
                progress: FifoProgress::default(),
                cmd_index: cmd.index,
                phase: Phase::DataRead,
                stage: BlockRequestStage::Command,
                stop_after_complete: false,
                response: None,
            },
        });
        for index in 0..128 {
            mmio[FIFO_WORD + index] = index as u32;
        }
        unsafe {
            mmio.as_mut_ptr()
                .add(STATUS_WORD)
                .write_volatile(Status::new().with_fifo_count(128).into_bits());
        }
        host.enable_completion_irq();
        host.data_cmd_index = cmd.index;
        host.command_state = crate::command::CommandState::WaitingStart { cmd };
        host.irq.state.begin_request();
        let generation = host.irq.state.generation();
        host.irq.state.cache_if_current(
            generation,
            crate::MCI_INT_COMMAND_DONE | crate::MCI_INT_DATA_TRANSFER_OVER,
            0,
        );

        assert!(matches!(
            host.service_fifo_event(&mut request, id, &mut slot),
            Ok(DataCommandPoll::Pending)
        ));
        let Some(BlockRequest {
            inner: BlockRequestKind::FifoRead { progress, .. },
        }) = request.as_ref()
        else {
            panic!("FIFO request must remain owned after the bounded pass")
        };
        assert_eq!(progress.offset, 64 * core::mem::size_of::<u32>());

        host.irq
            .state
            .cache_if_current(generation, crate::MCI_INT_RXDR, 0);
        assert!(matches!(
            host.service_fifo_event(&mut request, id, &mut slot),
            Ok(DataCommandPoll::Complete(_))
        ));
        assert!(request.is_none());
        assert_eq!(host.irq.state.pending_status(), 0);
    }

    #[test]
    fn borrowed_host2_submit_rejects_owned_cpu_buffer() {
        let dma = DeviceDma::new_legacy(u64::MAX, &TEST_DMA);
        let buffer = CpuDmaBuffer::new_zero(
            &dma,
            NonZeroUsize::new(BLOCK_SIZE).unwrap(),
            BLOCK_SIZE,
            DmaDirection::FromDevice,
        )
        .unwrap();
        let phase = sdio_host2::DataPhase::owned_cpu(
            sdio_host2::DataDirection::Read,
            NonZeroU16::new(BLOCK_SIZE as u16).unwrap(),
            NonZeroU32::new(1).unwrap(),
            buffer,
        )
        .unwrap();
        let transaction = sdio_host2::Transaction::with_data(cmd17(0), phase);
        let mut mmio = [0u32; 256];
        let mut host = host_from_words(&mut mmio);

        let result = unsafe {
            <PhytiumMci as sdio_host2::SdioHost>::submit_transaction(&mut host, transaction)
        };

        assert!(matches!(result, Err(sdio_host2::Error::InvalidArgument)));
        assert_eq!(host.host2_active_id, None);
    }

    #[test]
    fn owned_host2_submit_returns_unsupported_cpu_buffer_unchanged() {
        let dma = DeviceDma::new_legacy(u64::MAX, &TEST_DMA);
        let buffer = CpuDmaBuffer::new_zero(
            &dma,
            NonZeroUsize::new(BLOCK_SIZE).unwrap(),
            BLOCK_SIZE,
            DmaDirection::FromDevice,
        )
        .unwrap();
        let original_ptr = buffer.cpu_ptr();
        let phase = sdio_host2::DataPhase::owned_cpu(
            sdio_host2::DataDirection::Read,
            NonZeroU16::new(BLOCK_SIZE as u16).unwrap(),
            NonZeroU32::new(1).unwrap(),
            buffer,
        )
        .unwrap();
        let transaction = sdio_host2::Transaction::with_data(cmd17(0), phase);
        let mut mmio = [0u32; 256];
        let mut host = host_from_words(&mut mmio);

        let error = match unsafe {
            <PhytiumMci as sdio_host2::SdioHost>::submit_transaction_owned(
                &mut host,
                transaction,
            )
        } {
            Ok(_) => panic!("Phytium MCI unexpectedly accepted an owned PIO buffer"),
            Err(error) => error,
        };

        assert_eq!(error.error, sdio_host2::Error::Unsupported);
        let transaction = error
            .into_transaction()
            .expect("unsupported owned submit must return the transaction");
        let phase = transaction
            .data
            .expect("returned transaction must retain its data phase");
        let sdio_host2::DataBuffer::OwnedCpu(buffer) = phase.buffer else {
            panic!("returned transaction substituted the CPU buffer")
        };
        assert_eq!(buffer.cpu_ptr(), original_ptr);
        assert_eq!(host.host2_active_id, None);
    }
}
