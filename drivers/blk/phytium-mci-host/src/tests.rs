use core::num::{NonZeroU16, NonZeroU32};

use sdmmc_protocol::{
    BlockTransferMode,
    cmd::CMD0,
    response::ResponseType,
    sdio::host::{ClockSpeed, SignalVoltage},
};

use crate::{
    PhytiumMci,
    command::encode_command,
    regs::{Ctrl, Uhs},
    timing::{MediaKind, TimingTable},
};

#[test]
fn sd_timing_table_matches_phytium_sd_values() {
    let init = TimingTable::for_speed(ClockSpeed::Identification, MediaKind::Sd).unwrap();
    assert_eq!(init.clk_div, 0x7e7dfa);
    assert_eq!(init.clk_src, 0x000502);
    assert!(init.use_hold);

    let hs = TimingTable::for_speed(ClockSpeed::HighSpeed, MediaKind::Sd).unwrap();
    assert_eq!(hs.clk_div, 0x030204);
    assert_eq!(hs.clk_src, 0x000502);
    assert!(hs.use_hold);
}

#[test]
fn mmc_timing_table_uses_mmc_specific_rates() {
    let default = TimingTable::for_speed(ClockSpeed::Default, MediaKind::Mmc).unwrap();
    assert_eq!(default.target_hz, 26_000_000);

    let high = TimingTable::for_speed(ClockSpeed::HighSpeed, MediaKind::Mmc).unwrap();
    assert_eq!(high.target_hz, 52_000_000);
}

#[test]
fn unsupported_sd_clock_modes_are_rejected() {
    assert!(TimingTable::for_host_speed(ClockSpeed::Sdr104).is_err());
}

#[test]
fn ctrl_register_bits_match_phytium_mci_layout() {
    let reg = Ctrl::new()
        .with_int_enable(true)
        .with_dma_enable(true)
        .with_read_wait(true)
        .with_use_internal_dmac(true);

    assert_eq!(reg.into_bits(), (1 << 4) | (1 << 5) | (1 << 6) | (1 << 25));
}

#[test]
fn r3_command_encoding_does_not_enable_crc_check() {
    let cmd = sdmmc_protocol::cmd::Command::new(1, 0, ResponseType::R3);
    let reg = encode_command(&cmd, None);
    assert!(reg.response_expect());
    assert!(!reg.check_response_crc());
}

#[test]
fn cmd0_encoding_sends_initialization_clocks() {
    let reg = encode_command(&CMD0, None);
    assert!(reg.send_initialization());
    assert!(!reg.response_expect());
}

#[test]
fn cmd12_encoding_marks_stop_abort() {
    let reg = encode_command(&sdmmc_protocol::cmd::CMD12, None);
    assert!(reg.stop_abort_cmd());
}

#[test]
fn uhs_voltage_bit_tracks_signal_voltage() {
    let v180 = crate::host::uhs_bits_after_voltage(Uhs::new(), SignalVoltage::V180).unwrap();
    assert_eq!(v180.volt(), 1);

    let v330 = crate::host::uhs_bits_after_voltage(v180, SignalVoltage::V330).unwrap();
    assert_eq!(v330.volt(), 0);
}

#[test]
fn command_register_keeps_hold_register_optional() {
    let cmd = sdmmc_protocol::cmd::Command::new(17, 0, ResponseType::R1);
    let without_hold = encode_command(&cmd, None).with_use_hold_reg(false);
    assert!(!without_hold.use_hold_reg());
    assert_eq!(without_hold.cmd_index(), 17);
}

#[test]
fn host2_data_submit_reports_busy_without_dirtying_pending_data() {
    let mut host = unsafe { PhytiumMci::new_from_addr(0x1000_0000) };
    host.command_state = crate::command::CommandState::Issued {
        cmd: sdmmc_protocol::cmd::Command::new(0, 0, ResponseType::None),
    };
    let mut buf = [0u8; 512];
    let data = sdio_host2::DataPhase::read(
        NonZeroU16::new(512).unwrap(),
        NonZeroU32::new(1).unwrap(),
        &mut buf,
    )
    .unwrap();
    let tx = sdio_host2::Transaction::with_data(
        sdmmc_protocol::cmd::Command::new(17, 0, ResponseType::R1),
        data,
    );

    let err =
        match unsafe { <PhytiumMci as sdio_host2::SdioHost>::submit_transaction(&mut host, tx) } {
            Ok(_) => panic!("busy host accepted a second transaction"),
            Err(err) => err,
        };

    assert_eq!(err, sdio_host2::Error::Busy);
    assert!(host.pending_data.is_none());
    assert_eq!(host.data_blocks_remaining, 0);
}

#[test]
fn runtime_abort_retains_active_host2_request_until_lifecycle_quiescence() {
    use core::ptr::NonNull;

    let mut mmio = [0u32; 256];
    let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
    let mut host = unsafe { PhytiumMci::new(base) };
    host.enable_completion_irq();
    let transaction = sdio_host2::Transaction::command(sdmmc_protocol::cmd::Command::new(
        13,
        0,
        ResponseType::R1,
    ));
    let mut request =
        unsafe { <PhytiumMci as sdio_host2::SdioHost>::submit_transaction(&mut host, transaction) }
            .unwrap();
    let active_id = host.host2_active_id;

    assert_eq!(
        <PhytiumMci as sdio_host2::SdioHost>::abort_transaction(&mut host, &mut request),
        Err(sdio_host2::Error::Busy)
    );
    assert_eq!(host.host2_active_id, active_id);
}

#[test]
fn exposes_block_buffer_constraints() {
    let host = unsafe { PhytiumMci::new_from_addr(0x1000_0000) };

    let fifo = host.block_buffer_config(BlockTransferMode::Fifo);
    assert_eq!(fifo.block_size.get(), 512);
    assert_eq!(fifo.align, 1);
    assert_eq!(fifo.dma_mask, None);

    let dma = host.block_buffer_config(BlockTransferMode::Dma);
    assert_eq!(dma.block_size.get(), 512);
    assert_eq!(dma.align, 512);
    assert_eq!(dma.dma_mask, Some(u32::MAX as u64));
}

#[test]
fn timed_host2_reset_uses_absolute_deadline_independent_of_poll_count() {
    use core::ptr::NonNull;

    use sdmmc_protocol::sdio::host2::SdioHost2Timed;

    let mut mmio = [0u32; 256];
    let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
    let mut host = unsafe { PhytiumMci::new(base) };
    host.enable_completion_irq();
    let mut request = unsafe {
        <PhytiumMci as sdio_host2::SdioHost>::submit_bus_op(&mut host, sdio_host2::BusOp::ResetAll)
    }
    .unwrap();

    assert_eq!(
        <PhytiumMci as SdioHost2Timed>::poll_bus_op_at(&mut host, &mut request, 1_000),
        Ok(sdio_host2::RequestPoll::Pending)
    );
    assert_eq!(
        <PhytiumMci as SdioHost2Timed>::bus_op_wake_at(&host, &request),
        Some(51_000)
    );

    for _ in 0..128 {
        assert_eq!(
            <PhytiumMci as SdioHost2Timed>::poll_bus_op_at(&mut host, &mut request, 1_000),
            Ok(sdio_host2::RequestPoll::Pending)
        );
    }

    assert_eq!(
        <PhytiumMci as SdioHost2Timed>::poll_bus_op_at(&mut host, &mut request, 100_001_000,),
        Ok(sdio_host2::RequestPoll::Ready(Err(
            sdio_host2::Error::Timeout
        )))
    );
}

#[test]
fn host2_initialization_rejects_commands_before_completion_irq_is_bound() {
    use core::ptr::NonNull;

    let mut mmio = [0u32; 256];
    let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
    let mut host = unsafe { PhytiumMci::new(base) };

    assert!(matches!(
        unsafe {
            <PhytiumMci as sdio_host2::SdioHost>::submit_bus_op(
                &mut host,
                sdio_host2::BusOp::ResetAll,
            )
        },
        Err(sdio_host2::Error::Busy)
    ));
    assert_eq!(mmio[0], 0, "rejected init must not start controller reset");
    assert_eq!(mmio[11], 0, "rejected init must not issue a command");
}

#[test]
fn host2_reset_keeps_bound_completion_irq_across_eventless_reset_wait() {
    use core::ptr::NonNull;

    use sdmmc_protocol::sdio::host2::SdioHost2Timed;

    let mut mmio = [0u32; 256];
    let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
    let mut host = unsafe { PhytiumMci::new(base) };
    host.enable_completion_irq();
    let mut request = unsafe {
        <PhytiumMci as sdio_host2::SdioHost>::submit_bus_op(&mut host, sdio_host2::BusOp::ResetAll)
    }
    .unwrap();
    const CTRL_WORD: usize = 0;
    const INTMASK_WORD: usize = 9;

    assert_eq!(
        <PhytiumMci as SdioHost2Timed>::poll_bus_op_at(&mut host, &mut request, 1_000),
        Ok(sdio_host2::RequestPoll::Pending)
    );
    assert!(host.completion_irq_enabled());

    unsafe {
        mmio.as_mut_ptr().add(CTRL_WORD).write_volatile(0);
    }
    assert_eq!(
        <PhytiumMci as SdioHost2Timed>::poll_bus_op_at(&mut host, &mut request, 51_000),
        Ok(sdio_host2::RequestPoll::Pending)
    );
    assert!(host.completion_irq_enabled());
    assert_ne!(
        unsafe { mmio.as_ptr().add(INTMASK_WORD).read_volatile() },
        0
    );
    assert!(crate::regs::Ctrl::from_bits(mmio[CTRL_WORD]).int_enable());
}

#[test]
fn host2_reset_cleanup_waits_for_exclusive_irq_register_ownership() {
    use core::ptr::NonNull;

    use sdmmc_protocol::sdio::host2::SdioHost2Timed;

    const CTRL_WORD: usize = 0;
    const IDSTS_WORD: usize = 36;

    let mut mmio = [0u32; 256];
    let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
    let mut host = unsafe { PhytiumMci::new(base) };
    host.enable_completion_irq();
    let mut request = unsafe {
        <PhytiumMci as sdio_host2::SdioHost>::submit_bus_op(&mut host, sdio_host2::BusOp::ResetAll)
    }
    .unwrap();
    assert_eq!(
        <PhytiumMci as SdioHost2Timed>::poll_bus_op_at(&mut host, &mut request, 1_000),
        Ok(sdio_host2::RequestPoll::Pending)
    );
    unsafe {
        mmio.as_mut_ptr().add(CTRL_WORD).write_volatile(0);
        mmio.as_mut_ptr()
            .add(IDSTS_WORD)
            .write_volatile(crate::MCI_IDSTS_RECEIVE);
    }
    let irq_core = host.irq.clone();
    let irq_owner = irq_core
        .state
        .try_begin_irq_snapshot()
        .expect("test must model an in-flight IRQ snapshot");

    assert_eq!(
        <PhytiumMci as SdioHost2Timed>::poll_bus_op_at(&mut host, &mut request, 51_000),
        Ok(sdio_host2::RequestPoll::Pending)
    );
    assert_eq!(
        unsafe { mmio.as_ptr().add(IDSTS_WORD).read_volatile() },
        crate::MCI_IDSTS_RECEIVE,
        "initialization must not destructively acknowledge behind the IRQ owner"
    );

    drop(irq_owner);
    assert_eq!(
        <PhytiumMci as SdioHost2Timed>::poll_bus_op_at(&mut host, &mut request, 51_000),
        Ok(sdio_host2::RequestPoll::Pending)
    );
    assert_eq!(
        unsafe { mmio.as_ptr().add(IDSTS_WORD).read_volatile() },
        u32::MAX
    );
}
