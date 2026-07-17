use alloc::sync::Arc;
use core::{
    num::{NonZeroU16, NonZeroU32},
    sync::atomic::{AtomicUsize, Ordering as AtomicOrdering},
};

use sdio_host2::ResponseType;

use super::*;
use crate::irq::event_from_status;

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
fn event_classification_is_error_first_for_coalesced_status() {
    assert_eq!(
        event_from_status(NORMAL_INT_XFER_COMPLETE, ERROR_INT_DATA_CRC),
        Event::Error {
            normal: NORMAL_INT_XFER_COMPLETE,
            error: ERROR_INT_DATA_CRC,
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
fn card_sideband_irq_is_acknowledged_without_entering_request_epoch() {
    #[repr(align(4))]
    struct FakeRegs([u8; 0x100]);

    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.enable_completion_irq();
    assert!(host.irq.state.begin_request());
    host.write_u16(REG_NORMAL_INT_STATUS, NORMAL_INT_CARD_INTERRUPT);

    let event = host.irq_endpoint().handle_irq();

    assert_eq!(
        host.irq.state.pending_normal(),
        0,
        "controller sideband status must not contaminate a request generation"
    );
    assert_eq!(
        sdmmc_protocol::sdio::block_queue_ready_from_host_event(&event),
        None,
        "an acknowledged card sideband event must not schedule block request service"
    );
}

#[test]
fn exposes_block_buffer_constraints() {
    let host = unsafe { Sdhci::new_from_addr(0x1000_0000) };

    let dma = host.block_buffer_config(BlockTransferMode::Dma);
    assert_eq!(dma.block_size.get(), 512);
    assert_eq!(dma.align, 512);
    assert_eq!(dma.dma_mask, Some(u32::MAX as u64));
}

#[test]
fn host2_data_submit_reports_busy_without_dirtying_pending_data() {
    let mut host = unsafe { Sdhci::new_from_addr(0x1000_0000) };
    host.command_state = command::CommandState::Issued {
        cmd: Command::new(0, 0, ResponseType::None),
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
    assert!(host.pending_data.is_none());
}

#[test]
fn broadcom_low_speed_data_command_uses_absolute_write_gap() {
    #[repr(align(4))]
    struct FakeRegs([u8; 0x100]);

    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new_broadcom(base, BroadcomController::Bcm2835) };
    host.enable_completion_irq();
    host.bus_clock_hz = 400_000;
    let mut buffer = [0u8; 512];
    let data = sdio_host2::DataPhase::read(
        NonZeroU16::new(512).unwrap(),
        NonZeroU32::new(1).unwrap(),
        &mut buffer,
    )
    .unwrap();
    let transaction =
        sdio_host2::Transaction::with_data(Command::new(17, 0, ResponseType::R1), data);
    let mut request =
        unsafe { <Sdhci as sdio_host2::SdioHost>::submit_transaction(&mut host, transaction) }
            .unwrap();

    assert_eq!(host.read_u32(REG_BLOCK_SIZE), 0);
    assert_eq!(host.read_u32(REG_TRANSFER_MODE), 0);
    assert_eq!(
        <Sdhci as SdioHost2Timed>::poll_transaction_at(&mut host, &mut request, 1_000),
        Ok(sdio_host2::RequestPoll::Pending)
    );
    assert_ne!(host.read_u32(REG_BLOCK_SIZE), 0);
    assert_eq!(host.read_u32(REG_TRANSFER_MODE), 0);
    assert_eq!(
        <Sdhci as SdioHost2Timed>::transaction_wake_at(&host, &request),
        Some(11_000)
    );

    for now_ns in [1_000, 10_999] {
        assert_eq!(
            <Sdhci as SdioHost2Timed>::poll_transaction_at(&mut host, &mut request, now_ns,),
            Ok(sdio_host2::RequestPoll::Pending)
        );
        assert_eq!(host.read_u32(REG_TRANSFER_MODE), 0);
    }

    assert_eq!(
        <Sdhci as SdioHost2Timed>::poll_transaction_at(&mut host, &mut request, 11_000),
        Ok(sdio_host2::RequestPoll::Pending)
    );
    assert_ne!(host.read_u32(REG_TRANSFER_MODE), 0);
    assert_eq!(
        <Sdhci as SdioHost2Timed>::transaction_wake_at(&host, &request),
        None
    );
    assert!(matches!(
        host.command_state,
        command::CommandState::Issued { .. }
    ));
}

#[test]
fn broadcom_legacy_poll_fails_closed_during_required_write_gap() {
    #[repr(align(4))]
    struct FakeRegs([u8; 0x100]);

    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new_broadcom(base, BroadcomController::Bcm2835) };
    host.enable_completion_irq();
    host.bus_clock_hz = 400_000;
    let mut buffer = [0u8; 512];
    let data = sdio_host2::DataPhase::read(
        NonZeroU16::new(512).unwrap(),
        NonZeroU32::new(1).unwrap(),
        &mut buffer,
    )
    .unwrap();
    let transaction =
        sdio_host2::Transaction::with_data(Command::new(17, 0, ResponseType::R1), data);
    let mut request =
        unsafe { <Sdhci as sdio_host2::SdioHost>::submit_transaction(&mut host, transaction) }
            .unwrap();

    assert_eq!(
        <Sdhci as SdioHost2Timed>::poll_transaction_at(&mut host, &mut request, 1_000),
        Ok(sdio_host2::RequestPoll::Pending)
    );
    assert_eq!(
        <Sdhci as sdio_host2::SdioHost>::poll_transaction(&mut host, &mut request),
        Ok(sdio_host2::RequestPoll::Ready(Err(
            sdio_host2::Error::Unsupported
        )))
    );
    assert_eq!(host.read_u32(REG_TRANSFER_MODE), 0);
}

#[test]
fn host2_poll_after_complete_is_rejected() {
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
        <Sdhci as sdio_host2::SdioHost>::poll_bus_op(&mut host, &mut request),
        Ok(sdio_host2::RequestPoll::Ready(Ok(())))
    ));
    assert_eq!(
        <Sdhci as sdio_host2::SdioHost>::poll_bus_op(&mut host, &mut request),
        Err(sdio_host2::PollRequestError::AlreadyCompleted)
    );
}

#[test]
fn failed_runtime_abort_retains_the_active_host2_request() {
    #[repr(align(4))]
    struct FakeRegs([u8; 0x100]);

    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.enable_completion_irq();
    let transaction =
        sdio_host2::Transaction::command(Command::new(13, 0, sdio_host2::ResponseType::R1));
    let mut request =
        unsafe { <Sdhci as sdio_host2::SdioHost>::submit_transaction(&mut host, transaction) }
            .unwrap();
    let active_id = host.host2_active_id;

    assert_eq!(
        <Sdhci as sdio_host2::SdioHost>::abort_transaction(&mut host, &mut request),
        Err(sdio_host2::Error::Busy)
    );
    assert!(!request.done);
    assert_eq!(host.host2_active_id, active_id);
}

#[test]
fn failed_runtime_data_abort_retains_buffer_ownership() {
    #[repr(align(4))]
    struct FakeRegs([u8; 0x100]);

    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.enable_completion_irq();
    let mut buffer = [0u8; 512];
    let data = sdio_host2::DataPhase::read(
        NonZeroU16::new(512).unwrap(),
        NonZeroU32::new(1).unwrap(),
        &mut buffer,
    )
    .unwrap();
    let transaction =
        sdio_host2::Transaction::with_data(Command::new(17, 0, sdio_host2::ResponseType::R1), data);
    let mut request =
        unsafe { <Sdhci as sdio_host2::SdioHost>::submit_transaction(&mut host, transaction) }
            .unwrap();

    assert_eq!(
        <Sdhci as sdio_host2::SdioHost>::abort_transaction(&mut host, &mut request),
        Err(sdio_host2::Error::Busy)
    );
    assert!(
        request
            .data
            .as_ref()
            .and_then(|data| data.request.as_ref())
            .is_some()
    );
    assert!(!request.done);
    assert_eq!(host.host2_active_id, Some(request.id));
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
        <Sdhci as sdio_host2::SdioHost>::poll_bus_op(&mut host_b, &mut request),
        Err(sdio_host2::PollRequestError::WrongOwner)
    );
}

#[test]
fn host2_v180_requires_platform_voltage_capability() {
    let mut host = unsafe { Sdhci::new_from_addr(0x1000_0000) };

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
fn timed_host2_voltage_uses_caller_monotonic_time_without_host_timer() {
    #[repr(align(4))]
    struct FakeRegs([u8; 0x100]);

    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.enable_1v8_signaling();
    let mut request = unsafe {
        <Sdhci as sdio_host2::SdioHost>::submit_bus_op(
            &mut host,
            sdio_host2::BusOp::SetSignalVoltage(sdio_host2::SignalVoltage::V180),
        )
    }
    .expect("timed voltage switching must not require a second timer source");

    assert_eq!(
        <Sdhci as SdioHost2Timed>::poll_bus_op_at(&mut host, &mut request, 1_000),
        Ok(sdio_host2::RequestPoll::Pending)
    );
    assert_eq!(
        <Sdhci as SdioHost2Timed>::poll_bus_op_at(&mut host, &mut request, 1_000),
        Ok(sdio_host2::RequestPoll::Pending)
    );
    assert_eq!(
        <Sdhci as SdioHost2Timed>::bus_op_wake_at(&host, &request),
        Some(5_001_000)
    );

    for _ in 0..128 {
        assert_eq!(
            <Sdhci as SdioHost2Timed>::poll_bus_op_at(&mut host, &mut request, 1_000),
            Ok(sdio_host2::RequestPoll::Pending)
        );
    }
    assert_eq!(
        <Sdhci as SdioHost2Timed>::bus_op_wake_at(&host, &request),
        Some(5_001_000)
    );
}

#[test]
fn timed_host2_clock_timeout_depends_on_absolute_time_not_poll_count() {
    #[repr(align(4))]
    struct FakeRegs([u8; 0x100]);

    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.write_u32(REG_CAPABILITIES_LOW, 50 << 8);
    let mut request = unsafe {
        <Sdhci as sdio_host2::SdioHost>::submit_bus_op(
            &mut host,
            sdio_host2::BusOp::SetClock(ClockSpeed::Identification),
        )
    }
    .unwrap();

    assert_eq!(
        <Sdhci as SdioHost2Timed>::poll_bus_op_at(&mut host, &mut request, 1_000),
        Ok(sdio_host2::RequestPoll::Pending)
    );
    assert_eq!(
        <Sdhci as SdioHost2Timed>::bus_op_wake_at(&host, &request),
        Some(51_000)
    );
    for _ in 0..128 {
        assert_eq!(
            <Sdhci as SdioHost2Timed>::poll_bus_op_at(&mut host, &mut request, 1_000),
            Ok(sdio_host2::RequestPoll::Pending)
        );
    }
    assert_eq!(
        <Sdhci as SdioHost2Timed>::poll_bus_op_at(&mut host, &mut request, 100_001_000,),
        Ok(sdio_host2::RequestPoll::Ready(Err(
            sdio_host2::Error::Timeout
        )))
    );
}

#[test]
fn timed_host2_tuning_timeout_depends_on_absolute_time_not_poll_count() {
    #[repr(align(4))]
    struct FakeRegs([u8; 0x100]);

    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    let mut request = unsafe {
        <Sdhci as sdio_host2::SdioHost>::submit_bus_op(
            &mut host,
            sdio_host2::BusOp::ExecuteTuning {
                command: sdio_host2::Command::new(19, 0, ResponseType::R1),
                block_size: NonZeroU16::new(64).unwrap(),
            },
        )
    }
    .unwrap();

    assert_eq!(
        <Sdhci as SdioHost2Timed>::poll_bus_op_at(&mut host, &mut request, 1_000),
        Ok(sdio_host2::RequestPoll::Pending)
    );
    assert_eq!(
        <Sdhci as SdioHost2Timed>::bus_op_wake_at(&host, &request),
        Some(51_000)
    );
    for _ in 0..128 {
        assert_eq!(
            <Sdhci as SdioHost2Timed>::poll_bus_op_at(&mut host, &mut request, 1_000),
            Ok(sdio_host2::RequestPoll::Pending)
        );
    }
    assert_eq!(
        <Sdhci as SdioHost2Timed>::poll_bus_op_at(&mut host, &mut request, 100_001_000,),
        Ok(sdio_host2::RequestPoll::Ready(Err(
            sdio_host2::Error::Timeout
        )))
    );
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
        <Sdhci as sdio_host2::SdioHost>::poll_bus_op(&mut host, &mut request),
        Ok(sdio_host2::RequestPoll::Pending)
    ));
    assert!(matches!(
        <Sdhci as sdio_host2::SdioHost>::poll_bus_op(&mut host, &mut request),
        Ok(sdio_host2::RequestPoll::Ready(Err(
            sdio_host2::Error::Controller
        )))
    ));
}

#[test]
fn legacy_clock_trait_fails_closed_before_touching_external_clock() {
    #[repr(align(4))]
    struct FakeRegs([u8; 0x100]);

    struct Clock;

    impl HostClock for Clock {
        fn set_clock(&self, _target_hz: u32) -> Result<(), Error> {
            Ok(())
        }

        fn prepare_host_clock(&self, host: &mut Sdhci, target_hz: u32) -> Result<(), Error> {
            assert_eq!(target_hz, 400_000);
            assert_eq!(host.read_u16(REG_CLOCK_CONTROL) & CLOCK_SD_ENABLE, 0);
            host.write_u32(REG_CAPABILITIES_HIGH, 0xc10c);
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

    assert!(matches!(
        <Sdhci as ProtocolSdioHost>::set_clock(&mut host, ClockSpeed::Identification),
        Err(Error::UnsupportedCommand)
    ));

    assert_eq!(host.read_u32(REG_CAPABILITIES_HIGH), 0);
    assert_eq!(
        host.read_u16(REG_CLOCK_CONTROL),
        CLOCK_INTERNAL_ENABLE | CLOCK_INTERNAL_STABLE | CLOCK_SD_ENABLE
    );
}

#[test]
fn host2_external_clock_runs_host_stage_before_enable() {
    #[repr(align(4))]
    struct FakeRegs([u8; 0x100]);

    struct Clock;

    impl HostClock for Clock {
        fn set_clock(&self, target_hz: u32) -> Result<(), Error> {
            assert_eq!(target_hz, 375_000);
            Ok(())
        }

        fn effective_clock_hz(&self, target_hz: u32) -> u32 {
            assert_eq!(target_hz, 400_000);
            375_000
        }

        fn prepare_host_clock(&self, host: &mut Sdhci, target_hz: u32) -> Result<(), Error> {
            assert_eq!(target_hz, 375_000);
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
        <Sdhci as sdio_host2::SdioHost>::poll_bus_op(&mut host, &mut request),
        Ok(sdio_host2::RequestPoll::Pending)
    ));
    assert!(matches!(
        <Sdhci as sdio_host2::SdioHost>::poll_bus_op(&mut host, &mut request),
        Ok(sdio_host2::RequestPoll::Pending)
    ));
    assert!(matches!(
        <Sdhci as sdio_host2::SdioHost>::poll_bus_op(&mut host, &mut request),
        Ok(sdio_host2::RequestPoll::Pending)
    ));
    assert!(matches!(
        <Sdhci as sdio_host2::SdioHost>::poll_bus_op(&mut host, &mut request),
        Ok(sdio_host2::RequestPoll::Pending)
    ));
    assert_eq!(host.read_u16(REG_CLOCK_CONTROL), CLOCK_INTERNAL_ENABLE);
    host.write_u16(
        REG_CLOCK_CONTROL,
        host.read_u16(REG_CLOCK_CONTROL) | CLOCK_INTERNAL_STABLE,
    );
    assert!(matches!(
        <Sdhci as sdio_host2::SdioHost>::poll_bus_op(&mut host, &mut request),
        Ok(sdio_host2::RequestPoll::Ready(Ok(())))
    ));

    assert_eq!(host.read_u32(REG_CAPABILITIES_HIGH), 0x5d17);
    assert_ne!(host.read_u16(REG_CLOCK_CONTROL) & CLOCK_SD_ENABLE, 0);
    assert_eq!(host.active_bus_clock_hz(), 375_000);
}

#[test]
fn owned_irq_endpoint_acks_and_caches_status() {
    #[repr(align(4))]
    struct FakeRegs([u8; 0x100]);

    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.irq.state.begin_request();
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
    assert_eq!(handle.handle_irq(), Event::None);
}

#[test]
fn recovery_reset_uses_absolute_schedule_and_proves_quiescence_after_reset_clears() {
    #[repr(align(4))]
    struct FakeRegs([u8; 0x100]);

    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.enable_completion_irq();
    host.disable_completion_irq();
    let mut recovery = <Sdhci as SdioHost2Lifecycle>::begin_recovery(
        &mut host,
        rdif_block::RecoveryCause::QueueFault { queue_id: 0 },
    )
    .unwrap();

    let wake_at = match <Sdhci as SdioHost2Lifecycle>::poll_dma_quiesce(
        &mut host,
        &mut recovery,
        rdif_block::InitInput::at(1_000),
    ) {
        rdif_block::InitPoll::Pending(schedule) => schedule.wake_at_ns().unwrap(),
        _ => panic!("first pass must only arm RESET_ALL"),
    };
    assert!(wake_at > 1_000);
    assert_ne!(host.read_u8(REG_SOFTWARE_RESET) & RESET_ALL, 0);

    host.write_u8(REG_SOFTWARE_RESET, 0);
    assert!(matches!(
        <Sdhci as SdioHost2Lifecycle>::poll_dma_quiesce(
            &mut host,
            &mut recovery,
            rdif_block::InitInput::at(wake_at),
        ),
        rdif_block::InitPoll::Ready(())
    ));
    assert!(host.recovery_quiesced);
    assert!(host.initialization_status_owned());

    <Sdhci as SdioHost2Lifecycle>::begin_reinitialize(&mut host, &mut recovery).unwrap();
    assert!(matches!(
        <Sdhci as SdioHost2Lifecycle>::poll_reinitialize(
            &mut host,
            &mut recovery,
            rdif_block::InitInput::at(wake_at),
        ),
        rdif_block::InitPoll::Ready(())
    ));
    assert!(!host.recovery_quiesced);
    assert!(host.initialization_status_owned());

    host.enable_completion_irq();
    assert!(host.runtime_irq_status_owned());
}

#[test]
fn reset_restore_does_not_transfer_masked_runtime_status_ownership() {
    #[repr(align(4))]
    struct FakeRegs([u8; 0x100]);

    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.enable_completion_irq();
    host.disable_completion_irq();

    host.restore_completion_irq_after_reset(false);

    assert!(host.runtime_irq_status_owned());
    assert!(!host.completion_irq_enabled());
}

#[test]
fn recovery_rejects_platform_hook_without_bounded_callback_proof() {
    #[repr(align(4))]
    struct FakeRegs([u8; 0x100]);

    struct LegacyHook;

    impl HostResetHook for LegacyHook {
        fn after_reset(&self, _host: &mut Sdhci) -> Result<(), Error> {
            Ok(())
        }
    }

    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.set_reset_hook(LegacyHook);

    let software_reset_before = host.read_u8(REG_SOFTWARE_RESET);

    assert!(matches!(
        <Sdhci as SdioHost2Lifecycle>::begin_recovery(
            &mut host,
            rdif_block::RecoveryCause::QueueFault { queue_id: 0 },
        ),
        Err(Error::UnsupportedCommand)
    ));
    assert_eq!(host.read_u8(REG_SOFTWARE_RESET), software_reset_before);
}

#[test]
fn initial_reset_rejects_unbounded_hook_without_mmio_side_effects() {
    #[repr(align(4))]
    struct FakeRegs([u8; 0x100]);

    struct LegacyHook;

    impl HostResetHook for LegacyHook {
        fn before_reset_all(&self, host: &mut Sdhci) -> Result<(), Error> {
            host.write_u8(REG_POWER_CONTROL, 0xff);
            Ok(())
        }

        fn after_reset(&self, _host: &mut Sdhci) -> Result<(), Error> {
            Ok(())
        }
    }

    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.set_reset_hook(LegacyHook);
    let mut request = unsafe {
        <Sdhci as sdio_host2::SdioHost>::submit_bus_op(&mut host, sdio_host2::BusOp::ResetAll)
    }
    .unwrap();

    assert!(matches!(
        <Sdhci as sdio_host2::SdioHost>::poll_bus_op(&mut host, &mut request),
        Ok(sdio_host2::RequestPoll::Ready(Err(
            sdio_host2::Error::Unsupported
        )))
    ));
    assert_eq!(host.read_u8(REG_SOFTWARE_RESET), 0);
    assert_eq!(host.read_u8(REG_POWER_CONTROL), 0);
}

#[test]
fn scheduled_reset_hook_uses_an_absolute_deadline_without_busy_waiting() {
    #[repr(align(4))]
    struct FakeRegs([u8; 0x100]);

    #[derive(Default)]
    struct Trace {
        begin_calls: AtomicUsize,
        poll_calls: AtomicUsize,
    }

    struct ScheduledHook {
        trace: Arc<Trace>,
        wake_at_ns: Option<u64>,
    }

    impl HostResetHook for ScheduledHook {
        fn recovery_mode(&self) -> ResetHookRecoveryMode {
            ResetHookRecoveryMode::Scheduled
        }

        fn begin_before_reset_all(
            &mut self,
            _host: &mut Sdhci,
            now_ns: u64,
        ) -> Result<ResetHookPoll, Error> {
            self.trace.begin_calls.fetch_add(1, AtomicOrdering::Relaxed);
            let wake_at_ns = now_ns.saturating_add(1_000);
            self.wake_at_ns = Some(wake_at_ns);
            Ok(ResetHookPoll::Pending { wake_at_ns })
        }

        fn poll_before_reset_all(
            &mut self,
            _host: &mut Sdhci,
            now_ns: u64,
        ) -> Result<ResetHookPoll, Error> {
            let wake_at_ns = self.wake_at_ns.ok_or(Error::InvalidArgument)?;
            assert!(now_ns >= wake_at_ns);
            self.trace.poll_calls.fetch_add(1, AtomicOrdering::Relaxed);
            Ok(ResetHookPoll::Ready)
        }

        fn after_reset(&self, _host: &mut Sdhci) -> Result<(), Error> {
            Ok(())
        }
    }

    let trace = Arc::new(Trace::default());
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.set_reset_hook(ScheduledHook {
        trace: trace.clone(),
        wake_at_ns: None,
    });
    let mut request = unsafe {
        <Sdhci as sdio_host2::SdioHost>::submit_bus_op(&mut host, sdio_host2::BusOp::ResetAll)
    }
    .unwrap();

    assert_eq!(
        <Sdhci as SdioHost2Timed>::poll_bus_op_at(&mut host, &mut request, 100),
        Ok(sdio_host2::RequestPoll::Pending)
    );
    assert_eq!(
        <Sdhci as SdioHost2Timed>::bus_op_wake_at(&host, &request),
        Some(1_100)
    );
    assert_eq!(host.read_u8(REG_SOFTWARE_RESET), 0);
    assert_eq!(trace.begin_calls.load(AtomicOrdering::Relaxed), 1);
    assert_eq!(trace.poll_calls.load(AtomicOrdering::Relaxed), 0);

    assert_eq!(
        <Sdhci as SdioHost2Timed>::poll_bus_op_at(&mut host, &mut request, 500),
        Ok(sdio_host2::RequestPoll::Pending)
    );
    assert_eq!(host.read_u8(REG_SOFTWARE_RESET), 0);
    assert_eq!(trace.poll_calls.load(AtomicOrdering::Relaxed), 0);

    assert_eq!(
        <Sdhci as SdioHost2Timed>::poll_bus_op_at(&mut host, &mut request, 1_100),
        Ok(sdio_host2::RequestPoll::Pending)
    );
    assert_eq!(host.read_u8(REG_SOFTWARE_RESET), RESET_ALL);
    assert_eq!(trace.poll_calls.load(AtomicOrdering::Relaxed), 1);

    host.write_u8(REG_SOFTWARE_RESET, 0);
    assert_eq!(
        <Sdhci as SdioHost2Timed>::poll_bus_op_at(&mut host, &mut request, 1_150),
        Ok(sdio_host2::RequestPoll::Ready(Ok(())))
    );

    let mut recovery = <Sdhci as SdioHost2Lifecycle>::begin_recovery(
        &mut host,
        rdif_block::RecoveryCause::QueueFault { queue_id: 0 },
    )
    .unwrap();
    let rdif_block::InitPoll::Pending(schedule) = <Sdhci as SdioHost2Lifecycle>::poll_dma_quiesce(
        &mut host,
        &mut recovery,
        rdif_block::InitInput::at(2_000),
    ) else {
        panic!("scheduled recovery hook must publish its absolute wake")
    };
    assert_eq!(schedule.wake_at_ns(), Some(3_000));
    assert_eq!(host.read_u8(REG_SOFTWARE_RESET), 0);

    let rdif_block::InitPoll::Pending(schedule) = <Sdhci as SdioHost2Lifecycle>::poll_dma_quiesce(
        &mut host,
        &mut recovery,
        rdif_block::InitInput::at(2_500),
    ) else {
        panic!("early recovery poll must preserve the hook wake")
    };
    assert_eq!(schedule.wake_at_ns(), Some(3_000));
    assert_eq!(trace.poll_calls.load(AtomicOrdering::Relaxed), 1);

    assert!(matches!(
        <Sdhci as SdioHost2Lifecycle>::poll_dma_quiesce(
            &mut host,
            &mut recovery,
            rdif_block::InitInput::at(3_000),
        ),
        rdif_block::InitPoll::Pending(_)
    ));
    assert_eq!(host.read_u8(REG_SOFTWARE_RESET), RESET_ALL);
    assert_eq!(trace.poll_calls.load(AtomicOrdering::Relaxed), 2);
}

#[test]
fn aborting_a_scheduled_reset_invokes_platform_cancellation_before_mmio_reset() {
    #[repr(align(4))]
    struct FakeRegs([u8; 0x100]);

    struct Hook {
        cancel_calls: Arc<AtomicUsize>,
        invalid_deadline: bool,
    }

    impl HostResetHook for Hook {
        fn recovery_mode(&self) -> ResetHookRecoveryMode {
            ResetHookRecoveryMode::Scheduled
        }

        fn begin_before_reset_all(
            &mut self,
            _host: &mut Sdhci,
            now_ns: u64,
        ) -> Result<ResetHookPoll, Error> {
            let wake_at_ns = if self.invalid_deadline {
                now_ns
            } else {
                now_ns.saturating_add(1_000)
            };
            Ok(ResetHookPoll::Pending { wake_at_ns })
        }

        fn cancel_before_reset_all(&mut self, _host: &mut Sdhci) -> Result<(), Error> {
            self.cancel_calls.fetch_add(1, AtomicOrdering::Relaxed);
            Ok(())
        }

        fn after_reset(&self, _host: &mut Sdhci) -> Result<(), Error> {
            Ok(())
        }
    }

    let cancel_calls = Arc::new(AtomicUsize::new(0));
    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.set_reset_hook(Hook {
        cancel_calls: cancel_calls.clone(),
        invalid_deadline: false,
    });
    let mut request = unsafe {
        <Sdhci as sdio_host2::SdioHost>::submit_bus_op(&mut host, sdio_host2::BusOp::ResetAll)
    }
    .unwrap();

    assert!(matches!(
        <Sdhci as SdioHost2Timed>::poll_bus_op_at(&mut host, &mut request, 100),
        Ok(sdio_host2::RequestPoll::Pending)
    ));
    assert_eq!(
        <Sdhci as sdio_host2::SdioHost>::abort_bus_op(&mut host, &mut request),
        Err(sdio_host2::Error::Busy)
    );
    assert_eq!(cancel_calls.load(AtomicOrdering::Relaxed), 1);
    assert_eq!(host.read_u8(REG_SOFTWARE_RESET), 0);
    assert!(!request.done);

    let mut invalid_regs = FakeRegs([0; 0x100]);
    let invalid_base = NonNull::new(invalid_regs.0.as_mut_ptr()).unwrap();
    let mut invalid_host = unsafe { Sdhci::new(invalid_base) };
    invalid_host.set_reset_hook(Hook {
        cancel_calls: cancel_calls.clone(),
        invalid_deadline: true,
    });
    let mut invalid_request = unsafe {
        <Sdhci as sdio_host2::SdioHost>::submit_bus_op(
            &mut invalid_host,
            sdio_host2::BusOp::ResetAll,
        )
    }
    .unwrap();
    assert_eq!(
        <Sdhci as SdioHost2Timed>::poll_bus_op_at(&mut invalid_host, &mut invalid_request, 500,),
        Ok(sdio_host2::RequestPoll::Ready(Err(
            sdio_host2::Error::InvalidArgument
        )))
    );
    assert_eq!(cancel_calls.load(AtomicOrdering::Relaxed), 2);
    assert_eq!(invalid_host.read_u8(REG_SOFTWARE_RESET), 0);
}
