use core::ptr::NonNull;
use std::{boxed::Box, vec::Vec};

use rdif_serial::UartRegisterGate;

use super::*;

fn handle_irq(irq: &mut impl UartIrq) -> (Option<SerialIrqEvent>, Vec<RxSample>) {
    let Some(report) = irq.handle() else {
        return (None, Vec::new());
    };
    (Some(report.event), report.rx.as_slice().to_vec())
}

fn pl011_with_registers() -> (Box<Pl011Registers>, Pl011) {
    let mut regs = Box::new(unsafe { core::mem::zeroed::<Pl011Registers>() });
    let ptr = NonNull::from(regs.as_mut()).cast::<u8>();
    let uart = Pl011::new(ptr, 24_000_000);
    (regs, uart)
}

fn pl011_with_overrun_data() -> (Box<Pl011Registers>, Pl011) {
    let (regs, uart) = pl011_with_registers();
    regs.uartdr
        .set((UARTDR::DATA.val(0xab) + UARTDR::OE::SET).into());
    (regs, uart)
}

fn write_test_reg(regs: &mut Pl011Registers, offset: usize, value: u32) {
    unsafe {
        (regs as *mut Pl011Registers)
            .cast::<u32>()
            .add(offset / core::mem::size_of::<u32>())
            .write_volatile(value);
    }
}

fn read_test_reg(regs: &Pl011Registers, offset: usize) -> u32 {
    unsafe {
        (regs as *const Pl011Registers)
            .cast::<u32>()
            .add(offset / core::mem::size_of::<u32>())
            .read_volatile()
    }
}

fn started_parts(uart: Pl011) -> SerialParts<Pl011, Pl011Irq, Pl011EmergencyTx> {
    let mut parts = uart.split();
    parts.control.startup(&Config::new()).unwrap();
    parts
}

#[test]
fn raw_rx_reports_overrun_instead_of_swallowing_it() {
    let (_regs, mut uart) = pl011_with_overrun_data();

    let mut buf = [0];
    let err = uart
        .try_read(&mut buf)
        .expect_err("overrun must be reported to the caller");

    assert_eq!(buf[0], 0xab);
    assert_eq!(err.bytes_transferred, 1);
    assert_eq!(err.kind, TransferError::Overrun(0xab));
}

#[test]
fn raw_rx_sample_reports_overrun_instead_of_swallowing_it() {
    let (mut regs, uart) = pl011_with_overrun_data();
    let mut parts = uart.split();

    write_test_reg(&mut regs, 0x040, UARTIS::OE::SET.value);
    let (event, samples) = handle_irq(&mut parts.irq);
    let event = event.unwrap();
    assert!(event.events.contains(SerialEventSet::RX_STATUS));
    assert!(event.rx_errors.contains(RxErrorFlags::OVERRUN));
    assert_eq!(
        samples.len(),
        IRQ_RX_BATCH_CAPACITY,
        "the hard IRQ must enforce its RX budget"
    );
    let sample = samples[0];
    assert_eq!(sample.byte, Some(0xab));
    assert_eq!(sample.flag, RxFlag::Normal);
    assert!(sample.overrun);
}

#[test]
fn rx_irq_keeps_source_enabled_after_bounded_fifo_drain() {
    let (mut regs, uart) = pl011_with_registers();
    let mut irq = uart.split().irq;
    let rx_mask = imsc_for_events(SerialEventSet::RX);
    write_test_reg(&mut regs, 0x038, rx_mask);
    write_test_reg(&mut regs, 0x040, UARTIS::RX::SET.value);
    write_test_reg(&mut regs, 0x018, 0);
    regs.uartdr.set(UARTDR::DATA.val(b'r' as u32).into());

    let (event, samples) = handle_irq(&mut irq);
    let event = event.unwrap();

    assert!(event.events.contains(SerialEventSet::RX_DATA));
    assert!(!event.rearm.intersects(SerialEventSet::RX));
    assert_eq!(samples.len(), IRQ_RX_BATCH_CAPACITY);
    assert_eq!(read_test_reg(&regs, 0x038) & rx_mask, rx_mask);
}

#[test]
fn irq_status_without_rx_byte_is_preserved_after_irq_ack() {
    let (mut regs, uart) = pl011_with_registers();
    let mut parts = uart.split();

    write_test_reg(
        &mut regs,
        0x040,
        UARTIS::OE::SET.value | UARTIS::PE::SET.value,
    );
    write_test_reg(&mut regs, 0x018, UARTFR::RXFE::SET.value);

    let event = handle_irq(&mut parts.irq).0.unwrap();
    assert!(event.events.contains(SerialEventSet::RX_STATUS));
    assert!(event.rx_errors.contains(RxErrorFlags::PARITY));
    assert!(event.rx_errors.contains(RxErrorFlags::OVERRUN));
    assert!(parts.control.read_rx().is_none());
}

#[test]
fn tx_irq_exposes_space_without_owning_a_software_fifo() {
    let (mut regs, uart) = pl011_with_registers();
    let mut parts = started_parts(uart);

    write_test_reg(&mut regs, 0x018, 0);
    write_test_reg(&mut regs, 0x040, UARTIS::TX::SET.value);
    let event = handle_irq(&mut parts.irq).0.unwrap();
    assert!(event.events.contains(SerialEventSet::TX_SPACE));
    assert_eq!(parts.control.write_tx(b"x"), 1);
    assert_eq!(regs.uartdr.get() as u8, b'x');
}

#[test]
fn emergency_tx_returns_immediately_when_the_fifo_is_full() {
    let (mut regs, uart) = pl011_with_registers();
    let parts = uart.split();
    let gate = UartRegisterGate::new(parts.emergency_tx);
    let access = gate.try_enter().unwrap();
    write_test_reg(&mut regs, 0x018, UARTFR::TXFF::SET.value);

    assert_eq!(access.try_write(b"x"), 0);

    write_test_reg(&mut regs, 0x018, 0);
    assert_eq!(access.try_write(b"x"), 1);
    assert_eq!(regs.uartdr.get() as u8, b'x');
}

#[test]
fn emergency_tx_has_a_fixed_write_budget() {
    let (mut regs, uart) = pl011_with_registers();
    let parts = uart.split();
    write_test_reg(&mut regs, 0x018, 0);
    let bytes = [b'x'; 17];
    let gate = UartRegisterGate::new(parts.emergency_tx);
    let access = gate.try_enter().unwrap();

    assert_eq!(access.try_write(&bytes), 16);
}

#[test]
fn emergency_irq_mask_guard_restores_the_worker_mask() {
    let (mut regs, uart) = pl011_with_registers();
    let emergency = uart.split().emergency_tx;
    let enabled = UARTIS::RX::SET.value | UARTIS::TX::SET.value;
    write_test_reg(&mut regs, 0x038, enabled);

    let mask = emergency.mask_interrupts();
    assert_eq!(
        read_test_reg(&regs, 0x038),
        0,
        "a gate-busy IRQ must observe a device-masked emergency transaction"
    );
    drop(mask);
    assert_eq!(read_test_reg(&regs, 0x038), enabled);
}

#[test]
fn tx_irq_endpoint_acknowledges_tx_interrupt() {
    let (mut regs, uart) = pl011_with_registers();
    let mut irq = uart.split().irq;

    write_test_reg(&mut regs, 0x000, 0x5a);
    write_test_reg(&mut regs, 0x038, UARTIS::TX::SET.value);
    write_test_reg(&mut regs, 0x040, UARTIS::TX::SET.value);
    let event = handle_irq(&mut irq).0.unwrap();

    assert!(event.events.contains(SerialEventSet::TX_SPACE));
    assert_eq!(event.rearm, SerialEventSet::TX_SPACE);
    assert_eq!(
        read_test_reg(&regs, 0x044) & UARTIS::TX::SET.value,
        UARTIS::TX::SET.value
    );
    assert_eq!(read_test_reg(&regs, 0x038) & UARTIS::TX::SET.value, 0);
    assert_eq!(read_test_reg(&regs, 0x000), 0x5a);
}

#[test]
fn set_config_preserves_enabled_tx_and_rx_paths() {
    let (regs, mut uart) = pl011_with_registers();
    regs.uartcr
        .write(UARTCR::UARTEN::SET + UARTCR::TXE::SET + UARTCR::RXE::SET);

    uart.set_config(&Config::new()).unwrap();

    let cr = regs.uartcr.extract();
    assert!(cr.is_set(UARTCR::UARTEN));
    assert!(cr.is_set(UARTCR::TXE));
    assert!(cr.is_set(UARTCR::RXE));
}

#[test]
fn runtime_config_does_not_wait_for_the_transmitter_to_become_idle() {
    let (mut regs, mut uart) = pl011_with_registers();
    regs.uartcr
        .write(UARTCR::UARTEN::SET + UARTCR::TXE::SET + UARTCR::RXE::SET);
    write_test_reg(&mut regs, 0x018, UARTFR::BUSY::SET.value);

    assert_eq!(uart.set_config(&Config::new()), Ok(()));
    let cr = regs.uartcr.extract();
    assert!(cr.is_set(UARTCR::UARTEN));
    assert!(cr.is_set(UARTCR::TXE));
    assert!(cr.is_set(UARTCR::RXE));
}

#[test]
fn early_console_open_has_a_bounded_busy_failure() {
    let (mut regs, mut uart) = pl011_with_registers();
    let original_cr = (UARTCR::UARTEN::SET + UARTCR::TXE::SET + UARTCR::RXE::SET).value;
    write_test_reg(&mut regs, 0x030, original_cr);
    write_test_reg(&mut regs, 0x018, UARTFR::BUSY::SET.value);

    assert_eq!(uart.open(), Err(ConfigError::Timeout));
    assert_eq!(
        read_test_reg(&regs, 0x030),
        original_cr,
        "a failed early-console takeover must restore the previous control state"
    );
}

#[test]
fn rx_available_mask_enables_timeout_and_error_interrupts() {
    let (regs, mut uart) = pl011_with_registers();

    uart.set_irq_mask(SerialEventSet::RX);

    let imsc = regs.uartimsc.extract();
    assert!(imsc.is_set(UARTIS::RX));
    assert!(imsc.is_set(UARTIS::RT));
    assert!(imsc.is_set(UARTIS::FE));
    assert!(imsc.is_set(UARTIS::PE));
    assert!(imsc.is_set(UARTIS::BE));
    assert!(imsc.is_set(UARTIS::OE));
    assert_eq!(uart.get_irq_mask(), SerialEventSet::RX);
}

#[test]
fn hard_irq_does_not_claim_rx_ready_without_mis() {
    let (mut regs, uart) = pl011_with_registers();
    let mut parts = uart.split();

    parts.control.set_irq_mask(SerialEventSet::RX);
    write_test_reg(&mut regs, 0x040, 0);
    write_test_reg(&mut regs, 0x018, 0);

    assert!(handle_irq(&mut parts.irq).0.is_none());
}

#[test]
fn port_rx_ready_is_visible_without_irq_event() {
    let (mut regs, mut uart) = pl011_with_registers();

    uart.set_irq_mask(SerialEventSet::RX);
    write_test_reg(&mut regs, 0x040, 0);
    write_test_reg(&mut regs, 0x018, 0);
    regs.uartdr.set(UARTDR::DATA.val(b'r' as u32).into());

    let status = uart.poll_status();
    assert!(status.rx_ready());
    let sample = uart.read_rx().expect("RX sample should be available");
    assert_eq!(sample.byte, Some(b'r'));
    assert_eq!(sample.flag, RxFlag::Normal);
}

#[test]
fn rearm_remasks_rx_when_fifo_is_already_ready() {
    let (mut regs, mut uart) = pl011_with_registers();
    write_test_reg(&mut regs, 0x018, 0);

    let ready = uart.rearm(SerialEventSet::RX);

    assert_eq!(ready, SerialEventSet::RX_DATA);
    assert_eq!(
        read_test_reg(&regs, 0x038) & imsc_for_events(SerialEventSet::RX),
        0
    );
}

#[test]
fn unknown_irq_source_masks_all_without_fifo_access() {
    let (mut regs, uart) = pl011_with_registers();
    let mut irq = uart.split().irq;
    write_test_reg(&mut regs, 0x000, 0x5a);
    write_test_reg(&mut regs, 0x038, u32::MAX);
    write_test_reg(&mut regs, 0x040, 1 << 31);

    let event = handle_irq(&mut irq).0.unwrap();

    assert!(event.events.contains(SerialEventSet::FAULT));
    assert_eq!(read_test_reg(&regs, 0x038), 0);
    assert_eq!(read_test_reg(&regs, 0x000), 0x5a);
}
