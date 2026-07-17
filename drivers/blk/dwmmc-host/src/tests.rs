use core::num::{NonZeroU16, NonZeroU32};

use sdio_host2::ResponseType;

use super::*;
use crate::event::{DWMMC_IDMAC_INT_TI, event_from_raw_status};

#[test]
fn event_reports_command_completion_without_os_wakeup_policy() {
    let raw = crate::regs::RIntSts::new()
        .with_command_done(true)
        .into_bits();

    assert_eq!(event_from_raw_status(raw), Event::CommandComplete);
}

#[test]
fn event_reports_transfer_completion_without_os_wakeup_policy() {
    let raw = crate::regs::RIntSts::new()
        .with_data_transfer_over(true)
        .into_bits();

    assert_eq!(event_from_raw_status(raw), Event::TransferComplete);
}

#[test]
fn event_reports_error_status_without_translating_to_os_action() {
    let raw = crate::regs::RIntSts::new()
        .with_response_timeout(true)
        .into_bits();

    assert_eq!(event_from_raw_status(raw), Event::Error { raw_status: raw });
}

#[test]
fn event_reports_data_completion_source_for_runtime_wakeup() {
    use sdmmc_protocol::sdio::host::{HostEvent, HostEventKind, HostEventSource};

    let raw = crate::regs::RIntSts::new()
        .with_data_transfer_over(true)
        .into_bits();
    let event = event_from_raw_status(raw);

    assert_eq!(event.kind(), HostEventKind::TransferComplete);
    assert_eq!(event.source(), HostEventSource::Data);
    assert_eq!(event.queue_id(), Some(BlockRequestId::new(0)));
}

#[test]
fn exposes_block_buffer_constraints() {
    let mut mmio = [0u32; 256];
    let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
    let host = unsafe { DwMmc::new(base) };

    let dma = host.block_buffer_config(BlockTransferMode::Dma);
    assert_eq!(dma.block_size.get(), 512);
    assert_eq!(dma.align, 512);
    assert_eq!(dma.dma_mask, Some(u32::MAX as u64));
}

#[test]
fn host2_data_submit_reports_busy_without_dirtying_pending_data() {
    let mut mmio = [0u32; 256];
    let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
    let mut host = unsafe { DwMmc::new(base) };
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

    let err = match unsafe { <DwMmc as sdio_host2::SdioHost>::submit_transaction(&mut host, tx) } {
        Ok(_) => panic!("busy host accepted a second transaction"),
        Err(err) => err,
    };

    assert_eq!(err, sdio_host2::Error::Busy);
    assert!(host.pending_data.is_none());
    assert_eq!(host.data_blocks_remaining, 0);
}

#[test]
fn runtime_abort_retains_active_host2_request_until_lifecycle_quiescence() {
    let mut mmio = [0u32; 256];
    let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
    let mut host = unsafe { DwMmc::new(base) };
    host.enable_completion_irq();
    let transaction = sdio_host2::Transaction::command(Command::new(13, 0, ResponseType::R1));
    let mut request =
        unsafe { <DwMmc as sdio_host2::SdioHost>::submit_transaction(&mut host, transaction) }
            .unwrap();
    let active_id = host.host2_active_id;

    assert_eq!(
        <DwMmc as sdio_host2::SdioHost>::abort_transaction(&mut host, &mut request),
        Err(sdio_host2::Error::Busy)
    );
    assert_eq!(host.host2_active_id, active_id);
}

#[test]
fn owned_irq_endpoint_acks_and_caches_status() {
    let mut mmio = [0u32; 256];
    let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
    let mut host = unsafe { DwMmc::new(base) };
    host.irq.state.begin_request();
    let old_generation = host.irq.state.generation();
    let raw = crate::regs::RIntSts::new()
        .with_data_transfer_over(true)
        .into_bits();
    const MINTSTS_WORD: usize = 16;
    unsafe {
        mmio.as_mut_ptr().add(MINTSTS_WORD).write_volatile(raw);
    }

    let mut irq = host.irq_endpoint();

    assert_eq!(irq.handle_irq(), Event::TransferComplete);
    assert_eq!(host.irq.state.pending(), raw);
    unsafe {
        mmio.as_mut_ptr().add(MINTSTS_WORD).write_volatile(0);
    }
    assert_eq!(host.handle_irq(), Event::None);

    host.irq.state.end_request();
    host.irq.state.begin_request();
    assert_ne!(host.irq.state.generation(), old_generation);
    host.irq
        .state
        .cache_if_current(old_generation, crate::DWMMC_INT_DATA_TRANSFER_OVER);
    assert_eq!(host.irq.state.pending(), 0);
}

#[test]
fn irq_defers_destructive_status_snapshot_while_task_programs_request() {
    let mut mmio = [0u32; 256];
    let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
    let mut host = unsafe { DwMmc::new(base) };
    host.irq.state.begin_request();
    let irq_core = host.irq.clone();
    let task_owner = irq_core
        .state
        .try_begin_task_update()
        .expect("idle register gate must admit task setup");
    let raw = crate::regs::RIntSts::new()
        .with_data_transfer_over(true)
        .into_bits();
    const MINTSTS_WORD: usize = 16;
    unsafe {
        mmio.as_mut_ptr().add(MINTSTS_WORD).write_volatile(raw);
    }

    let mut irq = host.irq_endpoint();
    assert_eq!(irq.handle_irq(), Event::Deferred);
    assert_eq!(host.irq.state.pending(), 0);
    assert_eq!(
        unsafe { mmio.as_ptr().add(MINTSTS_WORD).read_volatile() },
        raw
    );

    drop(task_owner);
    assert_eq!(irq.handle_irq(), Event::TransferComplete);
    assert_eq!(host.irq.state.pending(), raw);
}

#[test]
fn task_register_update_defers_instead_of_spinning_behind_irq_snapshot() {
    let state = crate::host::IrqState::new();
    let irq_owner = state
        .try_begin_irq_snapshot()
        .expect("idle register gate must admit the IRQ snapshot");

    assert!(state.try_begin_task_update().is_none());

    drop(irq_owner);
    assert!(state.try_begin_task_update().is_some());
}

#[test]
fn idmac_completion_does_not_fabricate_controller_data_over() {
    let mut mmio = [0u32; 256];
    let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
    let mut host = unsafe { DwMmc::new(base) };
    const IDSTS_WORD: usize = 35;
    host.irq.state.begin_request();
    unsafe {
        mmio.as_mut_ptr()
            .add(IDSTS_WORD)
            .write_volatile(DWMMC_IDMAC_INT_TI);
    }

    let mut irq = host.irq_endpoint();

    assert_eq!(irq.handle_irq(), Event::DmaComplete);
    assert_eq!(
        host.irq.state.pending() & DWMMC_INT_DATA_TRANSFER_OVER,
        0,
        "IDMAC completion and controller DATA_OVER are independent hardware facts"
    );
    let cleared = unsafe { mmio.as_ptr().add(IDSTS_WORD).read_volatile() };
    assert_eq!(cleared, DWMMC_IDMAC_INT_TI);
}

#[test]
fn late_idmac_completion_cannot_cross_request_generation() {
    let state = crate::host::IrqState::new();
    state.begin_request();
    let stale_generation = state.generation();
    state.end_request();
    state.begin_request();

    state.cache_idmac_if_current(stale_generation, DWMMC_IDMAC_INT_TI);

    assert_eq!(state.pending_idmac(), 0);
}

#[test]
fn idmac_error_wins_over_a_combined_completion_snapshot() {
    use sdmmc_protocol::sdio::host::{HostEvent, HostEventKind, HostEventSource};

    let mut mmio = [0u32; 256];
    let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
    let mut host = unsafe { DwMmc::new(base) };
    const IDSTS_WORD: usize = 35;
    let idmac_status = crate::event::DWMMC_IDMAC_INT_FATAL_BUS_ERROR
        | crate::event::DWMMC_IDMAC_INT_ABNORMAL_SUMMARY
        | DWMMC_IDMAC_INT_TI;
    host.irq.state.begin_request();
    unsafe {
        mmio.as_mut_ptr()
            .add(IDSTS_WORD)
            .write_volatile(idmac_status);
    }

    let mut irq = host.irq_endpoint();

    let event = irq.handle_irq();
    assert_eq!(
        event,
        Event::DmaError {
            raw_status: idmac_status,
        }
    );
    assert_eq!(event.kind(), HostEventKind::Error);
    assert_eq!(event.source(), HostEventSource::Data);
    assert_eq!(event.queue_id(), Some(BlockRequestId::new(0)));
    assert_eq!(host.irq.state.pending(), 0);
    assert_ne!(
        host.irq.state.pending_idmac() & crate::event::DWMMC_IDMAC_INT_ERROR_MASK,
        0,
        "the worker must observe the DMA failure before any terminal completion"
    );
    assert_eq!(
        unsafe { mmio.as_ptr().add(IDSTS_WORD).read_volatile() },
        idmac_status,
        "the top half must acknowledge every observed IDMAC status bit"
    );
}

#[test]
fn completion_irq_uses_dma_mask_until_fifo_path_requests_fifo_irqs() {
    let mut mmio = [0u32; 256];
    let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
    let mut host = unsafe { DwMmc::new(base) };
    const INTMASK_WORD: usize = 9;
    let dma_mask = crate::DWMMC_INT_DATA_TRANSFER_OVER
        | crate::DWMMC_INT_COMMAND_DONE
        | crate::DWMMC_INT_ERROR_MASK;
    let fifo_mask = dma_mask | crate::DWMMC_INT_RXDR | crate::DWMMC_INT_TXDR;

    host.enable_completion_irq();

    let intmask = unsafe { mmio.as_ptr().add(INTMASK_WORD).read_volatile() };
    assert_eq!(intmask, dma_mask);

    host.program_fifo_interrupt_mask();

    let intmask = unsafe { mmio.as_ptr().add(INTMASK_WORD).read_volatile() };
    assert_eq!(intmask, fifo_mask);
}

#[test]
fn fifo_ready_irq_is_masked_until_task_side_drain() {
    let mut mmio = [0u32; 256];
    let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
    let mut host = unsafe { DwMmc::new(base) };
    const INTMASK_WORD: usize = 9;
    const MINTSTS_WORD: usize = 16;
    let fifo_ready = crate::DWMMC_INT_RXDR | crate::DWMMC_INT_TXDR;
    let fifo_mask = crate::DWMMC_INT_DATA_TRANSFER_OVER
        | crate::DWMMC_INT_COMMAND_DONE
        | crate::DWMMC_INT_ERROR_MASK
        | fifo_ready;

    host.enable_completion_irq();
    host.program_fifo_interrupt_mask();
    host.irq.state.begin_request();
    unsafe {
        mmio.as_mut_ptr()
            .add(MINTSTS_WORD)
            .write_volatile(fifo_ready);
    }

    let mut irq = host.irq_endpoint();

    assert_eq!(irq.handle_irq(), Event::ReceiveReady);
    assert_eq!(host.irq.state.pending(), fifo_ready);
    let intmask = unsafe { mmio.as_ptr().add(INTMASK_WORD).read_volatile() };
    assert_eq!(intmask, fifo_mask & !fifo_ready);

    host.program_fifo_interrupt_mask();

    let intmask = unsafe { mmio.as_ptr().add(INTMASK_WORD).read_volatile() };
    assert_eq!(intmask, fifo_mask);
}

#[test]
fn clear_all_int_status_matches_linux_w1c_all_bits() {
    let mut mmio = [0u32; 256];
    let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
    let host = unsafe { DwMmc::new(base) };
    const RINTSTS_WORD: usize = 17;
    unsafe {
        mmio.as_mut_ptr()
            .add(RINTSTS_WORD)
            .write_volatile(crate::DWMMC_INT_COMMAND_DONE);
    }

    host.clear_all_int_status();

    let written = unsafe { mmio.as_ptr().add(RINTSTS_WORD).read_volatile() };
    assert_eq!(written, u32::MAX);
}

#[test]
fn host2_reset_programs_linux_baseline_without_clock_update() {
    let mut mmio = [0u32; 256];
    let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
    let mut host = unsafe { DwMmc::new(base) };
    let mut request = unsafe {
        <DwMmc as sdio_host2::SdioHost>::submit_bus_op(&mut host, sdio_host2::BusOp::ResetAll)
    }
    .unwrap();
    const CTRL_WORD: usize = 0;
    const TMOUT_WORD: usize = 5;
    const CMD_WORD: usize = 11;
    const RINTSTS_WORD: usize = 17;
    const FIFOTH_WORD: usize = 19;
    const EXPECTED_FIFOTH: u32 = (0x2 << 28) | (0x7f << 16) | 0x80;

    assert!(matches!(
        <DwMmc as sdio_host2::SdioHost>::poll_bus_op(&mut host, &mut request).unwrap(),
        sdio_host2::RequestPoll::Pending
    ));
    unsafe {
        mmio.as_mut_ptr().add(CTRL_WORD).write_volatile(0);
    }
    assert!(matches!(
        <DwMmc as sdio_host2::SdioHost>::poll_bus_op(&mut host, &mut request).unwrap(),
        sdio_host2::RequestPoll::Ready(Ok(()))
    ));

    assert_eq!(
        unsafe { mmio.as_ptr().add(RINTSTS_WORD).read_volatile() },
        u32::MAX
    );
    assert_eq!(
        unsafe { mmio.as_ptr().add(TMOUT_WORD).read_volatile() },
        u32::MAX
    );
    assert_eq!(
        unsafe { mmio.as_ptr().add(FIFOTH_WORD).read_volatile() },
        EXPECTED_FIFOTH
    );
    assert_eq!(unsafe { mmio.as_ptr().add(CMD_WORD).read_volatile() }, 0);
}

#[test]
fn host2_reset_discards_pre_activation_idmac_status() {
    let mut mmio = [0u32; 256];
    let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
    let mut host = unsafe { DwMmc::new(base) };
    const CTRL_WORD: usize = 0;
    const IDSTS_WORD: usize = 35;
    mmio[IDSTS_WORD] = DWMMC_IDMAC_INT_TI;
    let mut request = unsafe {
        <DwMmc as sdio_host2::SdioHost>::submit_bus_op(&mut host, sdio_host2::BusOp::ResetAll)
    }
    .unwrap();

    assert!(matches!(
        <DwMmc as sdio_host2::SdioHost>::poll_bus_op(&mut host, &mut request).unwrap(),
        sdio_host2::RequestPoll::Pending
    ));
    mmio[CTRL_WORD] = 0;
    assert!(matches!(
        <DwMmc as sdio_host2::SdioHost>::poll_bus_op(&mut host, &mut request).unwrap(),
        sdio_host2::RequestPoll::Ready(Ok(()))
    ));

    assert_eq!(
        mmio[IDSTS_WORD],
        crate::event::DWMMC_IDMAC_INT_ENABLE_MASK,
        "reset completion owns the masked IRQ endpoint and must W1C stale IDMAC causes"
    );
}

#[test]
fn host2_reset_restores_bound_completion_irq_before_returning() {
    let mut mmio = [0u32; 256];
    let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
    let mut host = unsafe { DwMmc::new(base) };
    host.enable_completion_irq();
    let mut request = unsafe {
        <DwMmc as sdio_host2::SdioHost>::submit_bus_op(&mut host, sdio_host2::BusOp::ResetAll)
    }
    .unwrap();
    const CTRL_WORD: usize = 0;
    const INTMASK_WORD: usize = 9;

    assert!(matches!(
        <DwMmc as sdio_host2::SdioHost>::poll_bus_op(&mut host, &mut request).unwrap(),
        sdio_host2::RequestPoll::Pending
    ));
    assert!(host.completion_irq_enabled());

    unsafe {
        mmio.as_mut_ptr().add(CTRL_WORD).write_volatile(0);
    }
    assert!(matches!(
        <DwMmc as sdio_host2::SdioHost>::poll_bus_op(&mut host, &mut request).unwrap(),
        sdio_host2::RequestPoll::Ready(Ok(()))
    ));
    assert!(host.completion_irq_enabled());
    assert_ne!(
        unsafe { mmio.as_ptr().add(INTMASK_WORD).read_volatile() },
        0
    );
    assert!(crate::regs::Ctrl::from_bits(mmio[CTRL_WORD]).int_enable());
}

#[test]
fn host2_power_on_resets_after_enabling_pwren() {
    let mut mmio = [0u32; 256];
    let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
    let mut host = unsafe { DwMmc::new(base) };
    let mut request = unsafe {
        <DwMmc as sdio_host2::SdioHost>::submit_bus_op(&mut host, sdio_host2::BusOp::PowerOn)
    }
    .unwrap();
    const CTRL_WORD: usize = 0;
    const PWREN_WORD: usize = 1;
    const TMOUT_WORD: usize = 5;
    const RINTSTS_WORD: usize = 17;

    assert!(matches!(
        <DwMmc as sdio_host2::SdioHost>::poll_bus_op(&mut host, &mut request).unwrap(),
        sdio_host2::RequestPoll::Pending
    ));
    assert_eq!(unsafe { mmio.as_ptr().add(PWREN_WORD).read_volatile() }, 1);
    let ctrl =
        crate::regs::Ctrl::from_bits(unsafe { mmio.as_ptr().add(CTRL_WORD).read_volatile() });
    assert!(ctrl.controller_reset());
    assert!(ctrl.fifo_reset());
    assert!(ctrl.dma_reset());

    unsafe {
        mmio.as_mut_ptr().add(CTRL_WORD).write_volatile(0);
    }
    assert!(matches!(
        <DwMmc as sdio_host2::SdioHost>::poll_bus_op(&mut host, &mut request).unwrap(),
        sdio_host2::RequestPoll::Ready(Ok(()))
    ));
    assert_eq!(
        unsafe { mmio.as_ptr().add(RINTSTS_WORD).read_volatile() },
        u32::MAX
    );
    assert_eq!(
        unsafe { mmio.as_ptr().add(TMOUT_WORD).read_volatile() },
        u32::MAX
    );
}

#[test]
fn timed_host2_reset_uses_absolute_deadline_independent_of_poll_count() {
    use sdmmc_protocol::sdio::host2::SdioHost2Timed;

    let mut mmio = [0u32; 256];
    let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
    let mut host = unsafe { DwMmc::new(base) };
    let mut request = unsafe {
        <DwMmc as sdio_host2::SdioHost>::submit_bus_op(&mut host, sdio_host2::BusOp::ResetAll)
    }
    .unwrap();

    assert_eq!(
        <DwMmc as SdioHost2Timed>::poll_bus_op_at(&mut host, &mut request, 1_000),
        Ok(sdio_host2::RequestPoll::Pending)
    );
    assert_eq!(
        <DwMmc as SdioHost2Timed>::bus_op_wake_at(&host, &request),
        Some(51_000)
    );

    for _ in 0..128 {
        assert_eq!(
            <DwMmc as SdioHost2Timed>::poll_bus_op_at(&mut host, &mut request, 1_000),
            Ok(sdio_host2::RequestPoll::Pending)
        );
    }

    assert_eq!(
        <DwMmc as SdioHost2Timed>::poll_bus_op_at(&mut host, &mut request, 100_001_000,),
        Ok(sdio_host2::RequestPoll::Ready(Err(
            sdio_host2::Error::Timeout
        )))
    );
}

#[test]
fn absent_controller_card_detect_rejects_command_before_issue() {
    let mut mmio = [0u32; 256];
    let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
    let mut host = unsafe { DwMmc::new(base) };
    const CMD_WORD: usize = 11;
    const CDETECT_WORD: usize = 20;
    unsafe {
        mmio.as_mut_ptr().add(CDETECT_WORD).write_volatile(1);
    }

    let err = host
        .submit_command(&Command::new(8, 0x1aa, ResponseType::R7))
        .expect_err("absent card must not issue a command");

    assert_eq!(err, Error::NoCard);
    assert_eq!(unsafe { mmio.as_ptr().add(CMD_WORD).read_volatile() }, 0);
    assert!(matches!(host.command_state, command::CommandState::Idle));
}

#[test]
fn host2_set_clock_rewrites_clksrc_like_linux_setup_bus() {
    let mut mmio = [0u32; 256];
    let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
    let mut host = unsafe { DwMmc::new(base) };
    host.set_reference_clock(50_000_000);
    const CLKSRC_WORD: usize = 3;
    unsafe {
        mmio.as_mut_ptr()
            .add(CLKSRC_WORD)
            .write_volatile(0xdead_beef);
    }
    let mut request = unsafe {
        <DwMmc as sdio_host2::SdioHost>::submit_bus_op(
            &mut host,
            sdio_host2::BusOp::SetClock(sdio_host2::ClockSpeed::Identification),
        )
    }
    .unwrap();

    assert!(matches!(
        <DwMmc as sdio_host2::SdioHost>::poll_bus_op(&mut host, &mut request).unwrap(),
        sdio_host2::RequestPoll::Pending
    ));

    assert_eq!(unsafe { mmio.as_ptr().add(CLKSRC_WORD).read_volatile() }, 0);
}

#[test]
fn host2_external_clock_returned_bus_hz_feeds_dwmmc_divider() {
    struct Clock;

    impl HostClock for Clock {
        fn set_clock(&self, target_hz: u32) -> Result<u32, Error> {
            assert_eq!(target_hz, 400_000);
            Ok(400_000)
        }
    }

    let mut mmio = [0u32; 256];
    let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
    let mut host = unsafe { DwMmc::new(base) };
    host.set_reference_clock(50_000_000);
    host.set_external_clock(Clock);
    let mut request = unsafe {
        <DwMmc as sdio_host2::SdioHost>::submit_bus_op(
            &mut host,
            sdio_host2::BusOp::SetClock(sdio_host2::ClockSpeed::Identification),
        )
    }
    .unwrap();
    const CMD_WORD: usize = 11;
    const CLKDIV_WORD: usize = 2;

    assert!(matches!(
        <DwMmc as sdio_host2::SdioHost>::poll_bus_op(&mut host, &mut request).unwrap(),
        sdio_host2::RequestPoll::Pending
    ));
    assert_eq!(host.reference_clock(), 50_000_000);

    assert!(matches!(
        <DwMmc as sdio_host2::SdioHost>::poll_bus_op(&mut host, &mut request).unwrap(),
        sdio_host2::RequestPoll::Pending
    ));
    assert_eq!(host.reference_clock(), 400_000);
    unsafe {
        mmio.as_mut_ptr().add(CMD_WORD).write_volatile(0);
    }

    assert!(matches!(
        <DwMmc as sdio_host2::SdioHost>::poll_bus_op(&mut host, &mut request).unwrap(),
        sdio_host2::RequestPoll::Pending
    ));
    assert_eq!(unsafe { mmio.as_ptr().add(CLKDIV_WORD).read_volatile() }, 0);
}

#[test]
fn rintsts_error_includes_host_timeout_and_fifo_overrun() {
    assert!(crate::regs::RIntSts::new().with_host_timeout(true).error());
    assert!(
        crate::regs::RIntSts::new()
            .with_fifo_under_over_run(true)
            .error()
    );
}

#[test]
fn uhs_i_sdr_modes_keep_ddr_disabled() {
    let cur = UhsBits { ddr: 1, volt: 1 };

    assert_eq!(uhs_bits_after_speed(cur, ClockSpeed::Sdr50).ddr, 0);
    assert_eq!(uhs_bits_after_speed(cur, ClockSpeed::Sdr104).ddr, 0);
    assert_eq!(uhs_bits_after_speed(cur, ClockSpeed::Hs200).ddr, 0);
}

#[test]
fn ddr50_enables_ddr_mode_for_card0() {
    let cur = UhsBits { ddr: 0, volt: 1 };

    assert_eq!(
        uhs_bits_after_speed(cur, ClockSpeed::Ddr50),
        UhsBits { ddr: 1, volt: 1 }
    );
}

#[test]
fn uhs_i_voltage_switch_selects_1v8_for_card0() {
    let cur = UhsBits { ddr: 1, volt: 0 };

    assert_eq!(
        uhs_bits_after_voltage(cur, SignalVoltage::V180).unwrap(),
        UhsBits { ddr: 1, volt: 1 }
    );
    assert_eq!(
        uhs_bits_after_voltage(cur, SignalVoltage::V330).unwrap(),
        UhsBits { ddr: 1, volt: 0 }
    );
}

#[test]
fn unsupported_1v2_voltage_is_rejected() {
    assert_eq!(
        volt_mask_for_signal(SignalVoltage::V120).unwrap_err(),
        Error::UnsupportedCommand
    );
}

#[test]
fn data_command_index_is_recorded_for_diagnostics() {
    let mut mmio = [0u32; 256];
    let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
    let mut host = unsafe { DwMmc::new(base) };
    host.data_cmd_index = 6;

    assert_eq!(host.data_cmd_index, 6);
}

#[test]
fn recovery_reset_and_clock_restore_are_bounded_state_transitions() {
    let mut mmio = [0u32; 256];
    let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
    let mut host = unsafe { DwMmc::new(base) };
    let mut recovery = <DwMmc as SdioHost2Lifecycle>::begin_recovery(
        &mut host,
        rdif_block::RecoveryCause::QueueFault { queue_id: 0 },
    )
    .unwrap();

    let wake_at = match <DwMmc as SdioHost2Lifecycle>::poll_dma_quiesce(
        &mut host,
        &mut recovery,
        rdif_block::InitInput::at(1_000),
    ) {
        rdif_block::InitPoll::Pending(schedule) => schedule.wake_at_ns().unwrap(),
        _ => panic!("first pass must only arm controller reset"),
    };
    host.regs.ctrl().write(crate::regs::Ctrl::new());
    assert!(matches!(
        <DwMmc as SdioHost2Lifecycle>::poll_dma_quiesce(
            &mut host,
            &mut recovery,
            rdif_block::InitInput::at(wake_at),
        ),
        rdif_block::InitPoll::Ready(())
    ));
    assert!(host.recovery_quiesced);

    <DwMmc as SdioHost2Lifecycle>::begin_reinitialize(&mut host, &mut recovery).unwrap();
    assert!(matches!(
        <DwMmc as SdioHost2Lifecycle>::poll_reinitialize(
            &mut host,
            &mut recovery,
            rdif_block::InitInput::at(wake_at),
        ),
        rdif_block::InitPoll::Pending(_)
    ));
    assert!(
        host.regs.cmd().read().start_cmd(),
        "every recovery clock update must actually be handed to the CIU"
    );
    host.regs.cmd().write(crate::regs::Cmd::from_bits(0));
    assert!(matches!(
        <DwMmc as SdioHost2Lifecycle>::poll_reinitialize(
            &mut host,
            &mut recovery,
            rdif_block::InitInput::at(wake_at + 1),
        ),
        rdif_block::InitPoll::Pending(schedule) if schedule.run_again()
    ));
    assert!(matches!(
        <DwMmc as SdioHost2Lifecycle>::poll_reinitialize(
            &mut host,
            &mut recovery,
            rdif_block::InitInput::at(wake_at + 2),
        ),
        rdif_block::InitPoll::Pending(_)
    ));
    assert!(host.regs.cmd().read().start_cmd());
    host.regs.cmd().write(crate::regs::Cmd::from_bits(0));
    assert!(matches!(
        <DwMmc as SdioHost2Lifecycle>::poll_reinitialize(
            &mut host,
            &mut recovery,
            rdif_block::InitInput::at(wake_at + 3),
        ),
        rdif_block::InitPoll::Pending(schedule) if schedule.run_again()
    ));
    assert!(matches!(
        <DwMmc as SdioHost2Lifecycle>::poll_reinitialize(
            &mut host,
            &mut recovery,
            rdif_block::InitInput::at(wake_at + 4),
        ),
        rdif_block::InitPoll::Pending(_)
    ));
    assert!(host.regs.cmd().read().start_cmd());
    host.regs.cmd().write(crate::regs::Cmd::from_bits(0));
    assert!(matches!(
        <DwMmc as SdioHost2Lifecycle>::poll_reinitialize(
            &mut host,
            &mut recovery,
            rdif_block::InitInput::at(wake_at + 5),
        ),
        rdif_block::InitPoll::Ready(())
    ));
    assert!(!host.recovery_quiesced);
}
