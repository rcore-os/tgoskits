use core::num::{NonZeroU16, NonZeroU32};

use sdio_host2::{ProgressCause, RequestProgress, ResponseType};

use super::*;

#[test]
fn protocol_progress_contracts_are_closed_and_exhaustive() {
    fn command_state(progress: sdmmc_protocol::CommandProgress) -> bool {
        match progress {
            sdmmc_protocol::CommandProgress::Pending => false,
            sdmmc_protocol::CommandProgress::Complete => true,
        }
    }

    fn block_state(progress: sdmmc_protocol::BlockProgress) -> bool {
        match progress {
            sdmmc_protocol::BlockProgress::Pending => false,
            sdmmc_protocol::BlockProgress::Complete => true,
        }
    }

    assert!(!command_state(sdmmc_protocol::CommandProgress::Pending));
    assert!(command_state(sdmmc_protocol::CommandProgress::Complete));
    assert!(!block_state(sdmmc_protocol::BlockProgress::Pending));
    assert!(block_state(sdmmc_protocol::BlockProgress::Complete));
}

#[test]
fn irq_capability_trait_controls_hardware_signal_masks() {
    #[repr(align(4))]
    struct FakeRegs([u8; 0x100]);

    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };

    assert!(!sdmmc_protocol::sdio::SdioIrqHost::completion_irq_enabled(
        &host
    ));
    sdmmc_protocol::sdio::SdioIrqHost::enable_completion_irq(&mut host).unwrap();
    assert!(sdmmc_protocol::sdio::SdioIrqHost::completion_irq_enabled(
        &host
    ));
    assert_ne!(host.read_u16(REG_NORMAL_INT_SIGNAL_ENABLE), 0);
    assert_ne!(host.read_u16(REG_ERROR_INT_SIGNAL_ENABLE), 0);

    sdmmc_protocol::sdio::SdioIrqHost::disable_completion_irq(&mut host).unwrap();
    assert!(!sdmmc_protocol::sdio::SdioIrqHost::completion_irq_enabled(
        &host
    ));
    assert_eq!(host.read_u16(REG_NORMAL_INT_SIGNAL_ENABLE), 0);
    assert_eq!(host.read_u16(REG_ERROR_INT_SIGNAL_ENABLE), 0);
}

#[test]
fn event_reports_command_completion_without_os_wakeup_policy() {
    assert_eq!(
        event_from_status(NORMAL_INT_CMD_COMPLETE, 0),
        Event::CommandComplete
    );
}

#[test]
fn event_reports_data_completion_without_os_wakeup_policy() {
    assert_eq!(
        event_from_status(NORMAL_INT_XFER_COMPLETE, 0),
        Event::TransferComplete
    );
}

#[test]
fn event_reports_error_status_without_translating_to_os_action() {
    assert_eq!(
        event_from_status(NORMAL_INT_ERROR, ERROR_INT_DATA_TIMEOUT),
        Event::Error {
            normal: NORMAL_INT_ERROR,
            error: ERROR_INT_DATA_TIMEOUT,
        }
    );
}

#[test]
fn event_reports_data_completion_source_for_runtime_wakeup() {
    use sdmmc_protocol::sdio::host::{HostEvent, HostEventKind, HostEventSource};

    let event = event_from_status(NORMAL_INT_XFER_COMPLETE, 0);

    assert_eq!(event.kind(), HostEventKind::TransferComplete);
    assert_eq!(event.source(), HostEventSource::Data);
    assert_eq!(event.queue_id(), Some(BlockRequestId::new(0)));
}

#[test]
fn merged_command_and_data_irq_reports_queue_ready() {
    use sdmmc_protocol::sdio::host::{HostEvent, HostEventKind, HostEventSource};

    let event = event_from_status(NORMAL_INT_CMD_COMPLETE | NORMAL_INT_XFER_COMPLETE, 0);

    assert_eq!(event.kind(), HostEventKind::TransferComplete);
    assert_eq!(event.source(), HostEventSource::Data);
    assert_eq!(event.queue_id(), Some(BlockRequestId::new(0)));
}

#[test]
fn data_transaction_rejects_missing_dma_capability() {
    #[repr(align(4))]
    struct FakeRegs([u8; 0x100]);

    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    let mut buffer = [0_u8; 512];
    let command = Command::new(17, 0, ResponseType::R1);
    let data = sdio_host2::DataPhase::read(
        NonZeroU16::new(512).unwrap(),
        NonZeroU32::new(1).unwrap(),
        &mut buffer,
    )
    .unwrap();
    let transaction = sdio_host2::Transaction::with_data(command, data);

    assert!(matches!(
        unsafe { <Sdhci as sdio_host2::SdioHost>::submit_transaction(&mut host, transaction) },
        Err(sdio_host2::Error::Unsupported)
    ));
}

#[test]
fn host2_data_submit_reports_busy_without_replacing_the_active_command() {
    let mut host = unsafe { Sdhci::new_from_addr(0x1000_0000) };
    host.command_state = command::CommandState::Issued {
        cmd: Command::new(0, 0, ResponseType::None),
        data_line: false,
        polls: 0,
    };
    let mut buf = [0u8; 512];
    let data = sdio_host2::DataPhase::read(
        NonZeroU16::new(512).unwrap(),
        NonZeroU32::new(1).unwrap(),
        &mut buf,
    )
    .unwrap();
    let tx = sdio_host2::Transaction::with_data(Command::new(17, 0, ResponseType::R1), data);

    let err = match unsafe { <Sdhci as sdio_host2::SdioHost>::submit_transaction(&mut host, tx) } {
        Ok(_) => panic!("busy host accepted a second transaction"),
        Err(err) => err,
    };

    assert_eq!(err, sdio_host2::Error::Busy);
    assert!(matches!(
        host.command_state,
        command::CommandState::Issued { .. }
    ));
}

#[test]
fn host2_r1b_busy_release_advances_on_register_retry() {
    #[repr(align(4))]
    struct FakeRegs([u8; 0x100]);

    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.enable_interrupt_status_capture();
    host.enable_completion_irq();
    let transaction = sdio_host2::Transaction::command(sdmmc_protocol::cmd::cmd7(1));
    let mut request =
        unsafe { <Sdhci as sdio_host2::SdioHost>::submit_transaction(&mut host, transaction) }
            .unwrap();
    host.write_u16(REG_NORMAL_INT_STATUS, NORMAL_INT_CMD_COMPLETE);
    let mut irq = host.irq_endpoint();
    assert_eq!(irq.handle_irq(), Event::CommandComplete);

    assert_eq!(
        <Sdhci as sdio_host2::SdioHost>::advance_transaction(
            &mut host,
            &mut request,
            ProgressCause::AcknowledgedIrq,
        ),
        Ok(RequestProgress::RegisterPending {
            retry_after: SDHCI_REGISTER_RETRY_DELAY,
        })
    );
    assert_eq!(
        <Sdhci as sdio_host2::SdioHost>::advance_transaction(
            &mut host,
            &mut request,
            ProgressCause::RegisterRetry,
        ),
        Ok(RequestProgress::RegisterPending {
            retry_after: SDHCI_REGISTER_RETRY_DELAY,
        })
    );
    host.write_u32(REG_PRESENT_STATE, PRESENT_DAT0_LINE_SIGNAL_LEVEL);

    assert!(matches!(
        <Sdhci as sdio_host2::SdioHost>::advance_transaction(
            &mut host,
            &mut request,
            ProgressCause::RegisterRetry,
        ),
        Ok(RequestProgress::Complete(Ok(_)))
    ));
}

#[test]
fn host2_advance_after_complete_is_rejected() {
    #[repr(align(4))]
    struct FakeRegs([u8; 0x100]);

    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    let mut request = unsafe {
        <Sdhci as sdio_host2::SdioHost>::submit_bus_op(&mut host, sdio_host2::BusOp::PowerOn)
    }
    .unwrap();

    assert!(matches!(
        <Sdhci as sdio_host2::SdioHost>::advance_bus_op(
            &mut host,
            &mut request,
            ProgressCause::Submitted,
        ),
        Ok(RequestProgress::Complete(Ok(())))
    ));
    assert_eq!(
        <Sdhci as sdio_host2::SdioHost>::advance_bus_op(
            &mut host,
            &mut request,
            ProgressCause::RegisterRetry,
        ),
        Err(sdio_host2::AdvanceRequestError::AlreadyCompleted)
    );
}

#[test]
fn host2_bus_request_is_bound_to_originating_host() {
    #[repr(align(4))]
    struct FakeRegs([u8; 0x100]);

    let mut regs_a = FakeRegs([0; 0x100]);
    let mut regs_b = FakeRegs([0; 0x100]);
    let base_a = NonNull::new(regs_a.0.as_mut_ptr()).unwrap();
    let base_b = NonNull::new(regs_b.0.as_mut_ptr()).unwrap();
    let mut host_a = unsafe { Sdhci::new(base_a) };
    let mut host_b = unsafe { Sdhci::new(base_b) };
    let mut request = unsafe {
        <Sdhci as sdio_host2::SdioHost>::submit_bus_op(&mut host_a, sdio_host2::BusOp::PowerOn)
    }
    .unwrap();

    assert_eq!(
        <Sdhci as sdio_host2::SdioHost>::advance_bus_op(
            &mut host_b,
            &mut request,
            ProgressCause::Submitted,
        ),
        Err(sdio_host2::AdvanceRequestError::WrongOwner)
    );
}

#[test]
fn host2_v180_requires_real_timer() {
    let mut host = unsafe { Sdhci::new_from_addr(0x1000_0000) };
    host.enable_1v8_signaling();

    assert!(matches!(
        unsafe {
            <Sdhci as sdio_host2::SdioHost>::submit_bus_op(
                &mut host,
                sdio_host2::BusOp::SetSignalVoltage(sdio_host2::SignalVoltage::V180),
            )
        },
        Err(sdio_host2::Error::Unsupported)
    ));
}

#[test]
fn host2_v180_rejects_partial_high_dat_lines_before_switch() {
    #[repr(align(4))]
    struct FakeRegs([u8; 0x100]);

    struct StaticTimer;

    impl HostTimer for StaticTimer {
        fn now_ms(&self) -> u64 {
            0
        }
    }

    static TIMER: StaticTimer = StaticTimer;

    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.enable_1v8_signaling();
    host.set_timer(&TIMER);
    host.write_u32(REG_PRESENT_STATE, 1 << 20);
    let mut request = unsafe {
        <Sdhci as sdio_host2::SdioHost>::submit_bus_op(
            &mut host,
            sdio_host2::BusOp::SetSignalVoltage(sdio_host2::SignalVoltage::V180),
        )
    }
    .unwrap();

    assert!(matches!(
        <Sdhci as sdio_host2::SdioHost>::advance_bus_op(
            &mut host,
            &mut request,
            ProgressCause::Submitted,
        ),
        Ok(RequestProgress::RegisterPending { .. })
    ));
    assert!(matches!(
        <Sdhci as sdio_host2::SdioHost>::advance_bus_op(
            &mut host,
            &mut request,
            ProgressCause::RegisterRetry,
        ),
        Ok(RequestProgress::Complete(Err(
            sdio_host2::Error::Controller
        )))
    ));
}

#[test]
fn clock_div_zero_quirk_uses_nonzero_divider_for_low_external_clock() {
    assert_eq!(sdhci_clock_divisor_with_quirk(375_000, 375_000, false), 0);
    assert_eq!(sdhci_clock_divisor_with_quirk(375_000, 375_000, true), 1);
    assert_eq!(
        sdhci_clock_divisor_with_quirk(50_000_000, 50_000_000, true),
        0
    );
}

#[test]
fn host2_external_clock_runs_host_stage_before_enable() {
    #[repr(align(4))]
    struct FakeRegs([u8; 0x100]);

    struct Clock;

    impl HostClock for Clock {
        fn set_clock(&self, _target_hz: u32) -> Result<(), Error> {
            Ok(())
        }

        fn clock_div_zero_broken(&self) -> bool {
            true
        }

        fn prepare_host_clock(&self, host: &mut Sdhci, target_hz: u32) -> Result<(), Error> {
            assert_eq!(target_hz, 400_000);
            assert_eq!(host.read_u16(REG_CLOCK_CONTROL) & CLOCK_SD_ENABLE, 0);
            host.write_u32(REG_CAPABILITIES_HIGH, 0x5d17);
            Ok(())
        }
    }

    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.write_u16(
        REG_CLOCK_CONTROL,
        CLOCK_INTERNAL_ENABLE | CLOCK_INTERNAL_STABLE | CLOCK_SD_ENABLE,
    );
    host.set_external_clock(Clock);
    let mut request = unsafe {
        <Sdhci as sdio_host2::SdioHost>::submit_bus_op(
            &mut host,
            sdio_host2::BusOp::SetClock(ClockSpeed::Identification),
        )
    }
    .unwrap();

    assert!(matches!(
        <Sdhci as sdio_host2::SdioHost>::advance_bus_op(
            &mut host,
            &mut request,
            ProgressCause::Submitted,
        ),
        Ok(RequestProgress::RegisterPending { .. })
    ));
    assert!(matches!(
        <Sdhci as sdio_host2::SdioHost>::advance_bus_op(
            &mut host,
            &mut request,
            ProgressCause::RegisterRetry,
        ),
        Ok(RequestProgress::RegisterPending { .. })
    ));
    assert!(matches!(
        <Sdhci as sdio_host2::SdioHost>::advance_bus_op(
            &mut host,
            &mut request,
            ProgressCause::RegisterRetry,
        ),
        Ok(RequestProgress::RegisterPending { .. })
    ));
    assert!(matches!(
        <Sdhci as sdio_host2::SdioHost>::advance_bus_op(
            &mut host,
            &mut request,
            ProgressCause::RegisterRetry,
        ),
        Ok(RequestProgress::RegisterPending { .. })
    ));
    assert_eq!(host.read_u16(REG_CLOCK_CONTROL), CLOCK_INTERNAL_ENABLE);
    host.write_u16(
        REG_CLOCK_CONTROL,
        host.read_u16(REG_CLOCK_CONTROL) | CLOCK_INTERNAL_STABLE,
    );
    assert!(matches!(
        <Sdhci as sdio_host2::SdioHost>::advance_bus_op(
            &mut host,
            &mut request,
            ProgressCause::RegisterRetry,
        ),
        Ok(RequestProgress::Complete(Ok(())))
    ));

    assert_eq!(host.read_u32(REG_CAPABILITIES_HIGH), 0x5d17);
    assert_ne!(host.read_u16(REG_CLOCK_CONTROL) & CLOCK_SD_ENABLE, 0);
}

#[test]
fn owned_irq_endpoint_acks_and_caches_status() {
    #[repr(align(4))]
    struct FakeRegs([u8; 0x100]);

    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.irq.state.begin_request();
    host.enable_interrupt_status_capture();
    host.enable_completion_irq();
    host.write_u16(REG_NORMAL_INT_STATUS, NORMAL_INT_ERROR);
    host.write_u16(REG_ERROR_INT_STATUS, ERROR_INT_DATA_TIMEOUT);

    let mut handle = host.irq_endpoint();

    assert_eq!(
        handle.handle_irq(),
        Event::Error {
            normal: NORMAL_INT_ERROR,
            error: ERROR_INT_DATA_TIMEOUT,
        }
    );
    assert_eq!(host.irq.state.pending_normal(), NORMAL_INT_ERROR);
    assert_eq!(host.irq.state.pending_error(), ERROR_INT_DATA_TIMEOUT);
    host.write_u16(REG_NORMAL_INT_STATUS, 0);
    host.write_u16(REG_ERROR_INT_STATUS, 0);
    assert_eq!(host.irq_endpoint().handle_irq(), Event::None);
}

#[test]
fn masked_irq_status_is_acked_without_publishing_an_event() {
    #[repr(align(4))]
    struct FakeRegs([u8; 0x100]);

    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.irq.state.begin_request();
    host.write_u16(REG_NORMAL_INT_STATUS, NORMAL_INT_XFER_COMPLETE);

    let mut handle = host.irq_endpoint();

    assert_eq!(handle.handle_irq(), Event::None);
    assert_eq!(host.irq.state.pending_normal(), 0);
}
