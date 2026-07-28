use core::num::{NonZeroU16, NonZeroU32};

use sdio_host2::ResponseType;

use super::*;
use crate::block_path::{SelectedDataPath, select_block_data_path};

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
fn required_adma2_policy_rejects_fifo_fallback_without_dma() {
    #[repr(align(4))]
    struct FakeRegs([u8; 0x100]);

    let mut regs = FakeRegs([0; 0x100]);
    let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.set_block_transfer_policy(BlockTransferPolicy::RequireAdma2);
    let mut buffer = [0_u8; 512];
    let command = Command::new(17, 0, ResponseType::R1);

    assert!(matches!(
        <Sdhci as ProtocolSdioHost>::submit_read_data(&mut host, &command, &mut buffer, 512, 1,),
        Err(Error::UnsupportedCommand)
    ));
}

#[test]
fn required_adma2_policy_accepts_mmc_ext_csd_data_command() {
    assert_eq!(
        select_block_data_path(
            BlockTransferPolicy::RequireAdma2,
            true,
            &sdmmc_protocol::cmd::CMD8_MMC,
            512,
            1,
            512,
            DataDirection::Read,
        ),
        Ok(SelectedDataPath::Adma2)
    );
}

#[test]
fn host2_data_submit_reports_busy_without_dirtying_pending_data() {
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
    assert!(host.pending_data.is_none());
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
fn clock_div_zero_quirk_uses_nonzero_divider_for_low_external_clock() {
    assert_eq!(sdhci_clock_divisor_with_quirk(375_000, 375_000, false), 0);
    assert_eq!(sdhci_clock_divisor_with_quirk(375_000, 375_000, true), 1);
    assert_eq!(
        sdhci_clock_divisor_with_quirk(50_000_000, 50_000_000, true),
        0
    );
}

#[test]
fn external_clock_host_stage_runs_before_sd_clock_output_is_reenabled() {
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
        Err(Error::Timeout(_))
    ));

    assert_eq!(host.read_u32(REG_CAPABILITIES_HIGH), 0xc10c);
    assert_eq!(host.read_u16(REG_CLOCK_CONTROL), CLOCK_INTERNAL_ENABLE);
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
    assert_eq!(host.handle_irq(), Event::None);
}
