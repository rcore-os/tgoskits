#[cfg(test)]
mod tests {
    use alloc::alloc;
    use core::sync::atomic::{AtomicUsize, Ordering};

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
    fn task_service_consumes_only_irq_owned_fifo_snapshot() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { DwMmc::new(base) };
        const RINTSTS_WORD: usize = 17;
        let raw = crate::regs::RIntSts::new()
            .with_data_transfer_over(true)
            .with_receive_fifo_data_request(true)
            .with_transmit_fifo_data_request(true)
            .into_bits();
        unsafe { mmio.as_mut_ptr().add(RINTSTS_WORD).write_volatile(raw) };

        assert_eq!(host.take_data_irq_status(), 0);
        assert_eq!(unsafe { mmio.as_ptr().add(RINTSTS_WORD).read_volatile() }, raw);

        host.enable_completion_irq();
        host.irq.state.begin_request();
        let generation = host.irq.state.generation();
        host.irq.state.cache_if_current(generation, raw);
        assert_eq!(host.take_data_irq_status(), raw);

        let cleared = unsafe { mmio.as_ptr().add(RINTSTS_WORD).read_volatile() };
        assert_eq!(cleared, raw);
    }

    #[test]
    fn irq_service_consumes_cached_command_and_data_completion_in_one_pass() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { DwMmc::new(base) };
        let mut slot = BlockRequestSlot::default();
        let id = slot
            .start(BlockTransferMode::Fifo, BlockTransferDirection::Read)
            .unwrap();
        let mut buffer = [0u8; BLOCK_SIZE];
        let cmd = cmd17(0);
        host.enable_completion_irq();
        host.data_cmd_index = cmd.index;
        host.command_state = crate::command::CommandState::WaitingStart { cmd };
        host.irq.state.begin_request();
        let generation = host.irq.state.generation();
        host.irq.state.cache_if_current(
            generation,
            crate::DWMMC_INT_COMMAND_DONE | crate::DWMMC_INT_DATA_TRANSFER_OVER,
        );
        let mut request = Some(BlockRequest {
            inner: BlockRequestKind::FifoRead {
                id,
                buffer: NonNull::new(buffer.as_mut_ptr()).unwrap(),
                len: buffer.len(),
                offset: buffer.len(),
                cmd_index: cmd.index,
                phase: Phase::DataRead,
                stage: BlockRequestStage::Command,
                transfer_done: false,
                stop_after_complete: false,
                response: None,
            },
        });

        assert!(matches!(
            host.service_block_request_response(&mut request, id, &mut slot),
            Ok(DataCommandPoll::Complete(_))
        ));
        assert!(request.is_none());
        assert_eq!(host.irq.state.pending(), 0);
    }

    #[test]
    fn irq_enabled_task_service_cannot_consume_raw_status_before_top_half() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { DwMmc::new(base) };
        const RINTSTS_WORD: usize = 17;
        let raw = crate::regs::RIntSts::new()
            .with_data_transfer_over(true)
            .with_receive_fifo_data_request(true)
            .into_bits();

        host.enable_completion_irq();
        host.irq.state.begin_request();
        unsafe {
            mmio.as_mut_ptr().add(RINTSTS_WORD).write_volatile(raw);
        }

        assert_eq!(host.take_data_irq_status(), 0);
        assert_eq!(host.irq.state.pending(), 0);

        let untouched = unsafe { mmio.as_ptr().add(RINTSTS_WORD).read_volatile() };
        assert_eq!(untouched, raw);
    }

    #[test]
    fn runtime_data_setup_does_not_ack_raw_irq_status() {
        const RINTSTS_WORD: usize = 17;
        const IDSTS_WORD: usize = 35;
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { DwMmc::new(base) };
        let pending = crate::DWMMC_INT_DATA_TRANSFER_OVER;
        let pending_idmac = 1 << 1;
        mmio[RINTSTS_WORD] = pending;
        mmio[IDSTS_WORD] = pending_idmac;
        host.enable_completion_irq();

        host.prepare_data_irq_for_transfer();

        assert_eq!(mmio[RINTSTS_WORD], pending);
        assert_eq!(mmio[IDSTS_WORD], pending_idmac);
    }

    #[test]
    fn irq_runtime_submit_does_not_reset_dma_per_request() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { DwMmc::new(base) };
        let dma = DeviceDma::new_legacy(u64::MAX, &TEST_DMA);
        let mut descriptors = dma
            .coherent_array_zero_with_align::<IdmacDesc>(1, IDMAC_DESC_ALIGN)
            .unwrap();
        host.enable_completion_irq();

        host.submit_idmac_transfer_mapped(&cmd17(0), 1, 0x2000, &mut descriptors)
            .unwrap();

        let control = crate::regs::Ctrl::from_bits(mmio[0]);
        assert!(!control.controller_reset());
        assert!(!control.fifo_reset());
        assert!(!control.dma_reset());
        assert!(control.dma_enable());
        assert!(control.use_internal_dmac());
        const IDINTEN_WORD: usize = 36;
        assert_eq!(
            mmio[IDINTEN_WORD] & crate::event::DWMMC_IDMAC_INT_ERROR_MASK,
            crate::event::DWMMC_IDMAC_INT_ERROR_MASK,
            "fatal IDMAC causes must activate the same IRQ-only recovery path"
        );
    }

    #[test]
    fn busy_runtime_data_command_is_rejected_before_idmac_is_armed() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { DwMmc::new(base) };
        let dma = DeviceDma::new_legacy(u64::MAX, &TEST_DMA);
        let mut descriptors = dma
            .coherent_array_zero_with_align::<IdmacDesc>(1, IDMAC_DESC_ALIGN)
            .unwrap();
        const STATUS_WORD: usize = 18;
        unsafe {
            mmio.as_mut_ptr()
                .add(STATUS_WORD)
                .write_volatile(crate::regs::Status::new().with_data_busy(true).into_bits());
        }
        host.enable_completion_irq();

        assert_eq!(
            host.submit_idmac_transfer_mapped(&cmd17(0), 1, 0x2000, &mut descriptors),
            Err(Error::Busy)
        );
        let control = crate::regs::Ctrl::from_bits(mmio[0]);
        assert!(!control.dma_enable());
        assert!(!control.use_internal_dmac());
    }

    #[test]
    fn dma_commit_point_releases_rejected_backing_and_quarantines_accepted_backing() {
        static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
        static DEALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

        struct CountingDma;

        impl dma_api::DmaOp for CountingDma {
            unsafe fn alloc_contiguous(
                &self,
                _constraints: dma_api::DmaConstraints,
                layout: core::alloc::Layout,
            ) -> Option<dma_api::DmaAllocHandle> {
                let ptr = NonNull::new(unsafe { alloc::alloc_zeroed(layout) })?;
                ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
                Some(unsafe {
                    dma_api::DmaAllocHandle::new(ptr, 0x1000_u64.into(), layout)
                })
            }

            unsafe fn dealloc_contiguous(&self, handle: dma_api::DmaAllocHandle) {
                DEALLOCATIONS.fetch_add(1, Ordering::Relaxed);
                unsafe { alloc::dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
            }

            unsafe fn alloc_coherent(
                &self,
                _constraints: dma_api::DmaConstraints,
                layout: core::alloc::Layout,
            ) -> Option<dma_api::DmaAllocHandle> {
                let ptr = NonNull::new(unsafe { alloc::alloc_zeroed(layout) })?;
                ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
                Some(unsafe {
                    dma_api::DmaAllocHandle::new(ptr, 0x2000_u64.into(), layout)
                })
            }

            unsafe fn dealloc_coherent(&self, handle: dma_api::DmaAllocHandle) {
                DEALLOCATIONS.fetch_add(1, Ordering::Relaxed);
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
                    dma_api::DmaMapHandle::new(addr, 0x3000_u64.into(), layout, None)
                })
            }

            unsafe fn unmap_streaming(&self, _handle: dma_api::DmaMapHandle) {}

            fn page_size(&self) -> usize {
                4096
            }
        }

        static DMA: CountingDma = CountingDma;

        ALLOCATIONS.store(0, Ordering::Relaxed);
        DEALLOCATIONS.store(0, Ordering::Relaxed);
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { DwMmc::new(base) };
        const STATUS_WORD: usize = 18;
        unsafe {
            mmio.as_mut_ptr().add(STATUS_WORD).write_volatile(
                crate::regs::Status::new()
                    .with_data_busy(true)
                    .into_bits(),
            );
        }
        host.enable_completion_irq();
        let dma = DeviceDma::new_legacy(u64::MAX, &DMA);
        let mut destination = [0u8; BLOCK_SIZE];
        let mut slot = BlockRequestSlot::default();

        assert!(matches!(
            host.submit_read_blocks(
                0,
                NonNull::new(destination.as_mut_ptr()).unwrap(),
                NonZeroUsize::new(destination.len()).unwrap(),
                Some(&dma),
                BlockTransferMode::Dma,
                &mut slot,
            ),
            Err(Error::Busy)
        ));
        assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), 2);
        assert_eq!(
            DEALLOCATIONS.load(Ordering::Relaxed),
            ALLOCATIONS.load(Ordering::Relaxed),
            "hardware rejected the request before ownership transfer, so no backing may be quarantined"
        );

        ALLOCATIONS.store(0, Ordering::Relaxed);
        DEALLOCATIONS.store(0, Ordering::Relaxed);
        unsafe {
            mmio.as_mut_ptr().add(STATUS_WORD).write_volatile(0);
        }
        let active = host
            .submit_read_blocks(
                0,
                NonNull::new(destination.as_mut_ptr()).unwrap(),
                NonZeroUsize::new(destination.len()).unwrap(),
                Some(&dma),
                BlockTransferMode::Dma,
                &mut slot,
            )
            .unwrap();
        assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), 2);

        drop(active);

        assert_eq!(
            DEALLOCATIONS.load(Ordering::Relaxed),
            0,
            "an unquiesced controller may still fetch both the data buffer and IDMAC descriptor table"
        );
    }

    #[test]
    fn admitted_data_command_activation_has_no_fallible_card_detect_window() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { DwMmc::new(base) };
        const CDETECT_WORD: usize = 20;
        host.enable_completion_irq();
        host.pending_data = Some(PendingData {
            direction: DataDirection::Read,
            block_size: BLOCK_SIZE as u32,
            block_count: 1,
        });
        host.ensure_runtime_data_command_can_issue().unwrap();
        let irq = host.irq.clone();
        let register_owner = irq
            .state
            .try_begin_task_update()
            .expect("admitted submit must own the register gate");

        // Removal after final admission is handled by the IRQ/watchdog path;
        // it must not return the buffer after IDMAC activation.
        unsafe {
            mmio.as_mut_ptr().add(CDETECT_WORD).write_volatile(1);
        }
        host.activate_admitted_data_command(&cmd17(0), &register_owner);

        assert!(matches!(
            host.command_state,
            crate::command::CommandState::WaitingStart { .. }
        ));
        assert!(host.regs.cmd().read().start_cmd());
    }

    #[test]
    fn fifo_read_latches_dto_until_buffer_is_drained() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { DwMmc::new(base) };
        const STATUS_WORD: usize = 18;
        const FIFO_WORD: usize = 128;
        let mut buffer = [0u8; 4];
        let mut offset = 0;
        let mut transfer_done = false;
        let dto = crate::regs::RIntSts::new()
            .with_data_transfer_over(true)
            .into_bits();

        host.enable_completion_irq();
        host.irq.state.begin_request();
        let generation = host.irq.state.generation();
        host.irq.state.cache_if_current(generation, dto);
        unsafe {
            mmio.as_mut_ptr()
                .add(STATUS_WORD)
                .write_volatile(crate::regs::Status::new().with_fifo_count(0).into_bits());
        }

        // A worker rerun without a new IRQ snapshot must not poll FIFO status.
        assert!(matches!(
            service_fifo_read_event(
                &mut host,
                NonNull::new(buffer.as_mut_ptr()).unwrap(),
                buffer.len(),
                &mut offset,
                &mut transfer_done,
                17,
                Phase::DataRead,
            ),
            Ok(BlockPoll::Pending)
        ));
        assert_eq!(offset, 0);
        assert!(transfer_done);

        unsafe {
            mmio.as_mut_ptr()
                .add(STATUS_WORD)
                .write_volatile(crate::regs::Status::new().with_fifo_count(1).into_bits());
            mmio.as_mut_ptr().add(FIFO_WORD).write_volatile(0x0403_0201);
        }

        assert!(matches!(
            service_fifo_read_event(
                &mut host,
                NonNull::new(buffer.as_mut_ptr()).unwrap(),
                buffer.len(),
                &mut offset,
                &mut transfer_done,
                17,
                Phase::DataRead,
            ),
            Ok(BlockPoll::Pending)
        ));
        assert_eq!(offset, 0);

        host.irq.state.cache_if_current(
            generation,
            crate::DWMMC_INT_RXDR,
        );
        assert!(matches!(
            service_fifo_read_event(
                &mut host,
                NonNull::new(buffer.as_mut_ptr()).unwrap(),
                buffer.len(),
                &mut offset,
                &mut transfer_done,
                17,
                Phase::DataRead,
            ),
            Ok(BlockPoll::Complete)
        ));
        assert_eq!(buffer, [1, 2, 3, 4]);
    }

    #[test]
    fn one_fifo_irq_service_is_limited_to_sixty_four_words() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { DwMmc::new(base) };
        const STATUS_WORD: usize = 18;
        const FIFO_WORD: usize = 128;
        let mut buffer = [0u8; 512];
        let mut offset = 0;
        let mut transfer_done = false;

        host.enable_completion_irq();
        host.irq.state.begin_request();
        let generation = host.irq.state.generation();
        host.irq.state.cache_if_current(
            generation,
            crate::DWMMC_INT_RXDR | crate::DWMMC_INT_DATA_TRANSFER_OVER,
        );
        unsafe {
            mmio.as_mut_ptr()
                .add(STATUS_WORD)
                .write_volatile(crate::regs::Status::new().with_fifo_count(128).into_bits());
            mmio.as_mut_ptr().add(FIFO_WORD).write_volatile(0x0403_0201);
        }

        assert_eq!(
            service_fifo_read_event(
                &mut host,
                NonNull::new(buffer.as_mut_ptr()).unwrap(),
                buffer.len(),
                &mut offset,
                &mut transfer_done,
                17,
                Phase::DataRead,
            ),
            Ok(BlockPoll::Pending)
        );
        assert_eq!(offset, 64 * core::mem::size_of::<u32>());
        assert!(transfer_done);
    }

    #[test]
    fn dma_completion_requires_idmac_then_controller_data_over() {
        let mut completion = DmaCompletionLatch::default();

        assert_eq!(
            completion.observe(0, crate::event::DWMMC_IDMAC_INT_TI),
            BlockPoll::Pending
        );
        assert_eq!(
            completion.observe(crate::DWMMC_INT_DATA_TRANSFER_OVER, 0),
            BlockPoll::Complete
        );
    }

    #[test]
    fn dma_completion_requires_controller_data_over_then_idmac() {
        let mut completion = DmaCompletionLatch::default();

        assert_eq!(
            completion.observe(crate::DWMMC_INT_DATA_TRANSFER_OVER, 0),
            BlockPoll::Pending
        );
        assert_eq!(
            completion.observe(0, crate::event::DWMMC_IDMAC_INT_TI),
            BlockPoll::Complete
        );
    }

    #[test]
    fn combined_idmac_and_controller_snapshot_completes_once() {
        let mut completion = DmaCompletionLatch::default();

        assert_eq!(
            completion.observe(
                crate::DWMMC_INT_DATA_TRANSFER_OVER,
                crate::event::DWMMC_IDMAC_INT_TI,
            ),
            BlockPoll::Complete
        );
        assert_eq!(completion.observe(0, 0), BlockPoll::Complete);
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
                dma_api::DmaAllocHandle::new(ptr, 0x1000_u64.into(), layout)
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
                dma_api::DmaAllocHandle::new(ptr, 0x1000_u64.into(), layout)
            })
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
}
