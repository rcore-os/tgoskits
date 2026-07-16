use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use arm_vpl011::{Pl011, Pl011Backend, Pl011BackendError, RxErrors, RxResult};
use axdevice_base::{
    AccessWidth, BusAccess, BusKind, BusResponse, ControllerInputId, Device, InterruptControllerId,
    InterruptTriggerMode, IrqResult, WiredIrqInput, WiredIrqSink,
};

const BASE: u64 = 0x0900_0000;
const DR: u64 = 0x000;
const RSR_ECR: u64 = 0x004;
const FR: u64 = 0x018;
const LCR_H: u64 = 0x02c;
const CR: u64 = 0x030;
const IFLS: u64 = 0x034;
const IMSC: u64 = 0x038;
const MIS: u64 = 0x040;

const FR_RXFE: u32 = 1 << 4;
const LCR_H_FEN: u32 = 1 << 4;
const CR_UARTEN: u32 = 1;
const CR_TXE: u32 = 1 << 8;
const CR_RXE: u32 = 1 << 9;
const INT_RX: u32 = 1 << 4;
const INT_TX: u32 = 1 << 5;
const INT_RT: u32 = 1 << 6;
const INT_PE: u32 = 1 << 8;
const INT_OE: u32 = 1 << 10;

#[test]
fn rx_fifo_threshold_drives_a_level_interrupt_until_drained() {
    let (uart, level, _) = test_uart();
    write(&uart, CR, CR_UARTEN | CR_RXE | CR_TXE);
    write(&uart, LCR_H, LCR_H_FEN);
    write(&uart, IFLS, 0); // RX threshold: 1/8 of the 16-byte FIFO.
    write(&uart, IMSC, INT_RX);

    assert_eq!(
        uart.receive(b'a', RxErrors::empty()).unwrap(),
        RxResult::Accepted
    );
    assert!(!level.load(Ordering::Acquire));
    assert_eq!(
        uart.receive(b'b', RxErrors::empty()).unwrap(),
        RxResult::Accepted
    );
    assert!(level.load(Ordering::Acquire));
    assert_eq!(read(&uart, MIS), INT_RX);

    assert_eq!(read(&uart, DR), b'a' as u32);
    assert!(!level.load(Ordering::Acquire));
    assert_eq!(read(&uart, DR), b'b' as u32);
    assert_ne!(read(&uart, FR) & FR_RXFE, 0);
}

#[test]
fn receive_timeout_and_error_interrupts_follow_pl011_clear_rules() {
    let (uart, level, _) = test_uart();
    write(&uart, CR, CR_UARTEN | CR_RXE);
    write(&uart, IMSC, INT_RT | INT_PE | INT_OE);

    assert_eq!(
        uart.receive(b'x', RxErrors::PARITY).unwrap(),
        RxResult::Accepted
    );
    assert_eq!(
        uart.receive(b'y', RxErrors::empty()).unwrap(),
        RxResult::DroppedOverrun
    );
    assert!(level.load(Ordering::Acquire));
    assert_eq!(read(&uart, MIS) & (INT_PE | INT_OE), INT_PE | INT_OE);

    write(&uart, RSR_ECR, 0);
    assert!(!level.load(Ordering::Acquire));
    uart.expire_receive_timeout().unwrap();
    assert_eq!(read(&uart, MIS), INT_RT);
    assert_eq!(read(&uart, DR) & 0xff, b'x' as u32);
    assert!(!level.load(Ordering::Acquire));
}

#[test]
fn buffered_backend_can_apply_fifo_backpressure_without_consuming_an_extra_byte() {
    let (uart, ..) = test_uart();
    write(&uart, CR, CR_UARTEN | CR_RXE);
    write(&uart, LCR_H, LCR_H_FEN);

    for byte in 0..16 {
        assert!(uart.receive_ready());
        assert_eq!(
            uart.receive(byte, RxErrors::empty()).unwrap(),
            RxResult::Accepted
        );
    }
    assert!(!uart.receive_ready());

    assert_eq!(read(&uart, DR), 0);
    assert!(uart.receive_ready());
}

#[test]
fn tx_interrupt_and_backend_are_independent_of_vcpu_state() {
    let (uart, level, output) = test_uart();
    write(&uart, CR, CR_UARTEN | CR_TXE);
    write(&uart, IMSC, INT_TX);
    assert!(level.load(Ordering::Acquire));

    write(&uart, DR, b'Z' as u32);

    assert_eq!(&*output.lock().unwrap(), b"Z");
    assert!(level.load(Ordering::Acquire));
    write(&uart, IMSC, 0);
    assert!(!level.load(Ordering::Acquire));
}

#[test]
fn byte_and_halfword_register_accesses_follow_the_apb_data_lanes() {
    let (uart, level, _) = test_uart();

    write_with_width(&uart, CR, CR_UARTEN | CR_RXE | CR_TXE, AccessWidth::Word);
    write_with_width(&uart, IMSC, INT_TX, AccessWidth::Word);
    assert!(level.load(Ordering::Acquire));
    assert_eq!(read_with_width(&uart, IMSC, AccessWidth::Word), INT_TX);

    write_with_width(&uart, IMSC, 0, AccessWidth::Byte);
    assert!(!level.load(Ordering::Acquire));
    assert_eq!(read_with_width(&uart, IMSC, AccessWidth::Byte), 0);
}

fn test_uart() -> (Pl011, Arc<AtomicBool>, Arc<Mutex<Vec<u8>>>) {
    let level = Arc::new(AtomicBool::new(false));
    let line = WiredIrqInput::new(
        InterruptControllerId::new(0),
        ControllerInputId::new(33),
        InterruptTriggerMode::LevelTriggered,
        Arc::new(LevelSink(level.clone())),
    )
    .connect()
    .unwrap();
    let output = Arc::new(Mutex::new(Vec::new()));
    let backend = Arc::new(RecordingBackend(output.clone()));
    (
        Pl011::new("console0", BASE, line, backend).unwrap(),
        level,
        output,
    )
}

fn read(uart: &Pl011, offset: u64) -> u32 {
    read_with_width(uart, offset, AccessWidth::Dword)
}

fn read_with_width(uart: &Pl011, offset: u64, width: AccessWidth) -> u32 {
    match uart
        .handle(&BusAccess {
            kind: BusKind::Mmio,
            is_read: true,
            addr: BASE + offset,
            width,
            data: 0,
        })
        .unwrap()
    {
        BusResponse::Read { value } => value as u32,
        BusResponse::Write => panic!("read returned a write response"),
    }
}

fn write(uart: &Pl011, offset: u64, value: u32) {
    write_with_width(uart, offset, value, AccessWidth::Dword);
}

fn write_with_width(uart: &Pl011, offset: u64, value: u32, width: AccessWidth) {
    uart.handle(&BusAccess {
        kind: BusKind::Mmio,
        is_read: false,
        addr: BASE + offset,
        width,
        data: u64::from(value),
    })
    .unwrap();
}

struct LevelSink(Arc<AtomicBool>);

impl WiredIrqSink for LevelSink {
    fn set_level(&self, _input: ControllerInputId, asserted: bool) -> IrqResult {
        self.0.store(asserted, Ordering::Release);
        Ok(())
    }

    fn pulse(&self, _input: ControllerInputId) -> IrqResult {
        unreachable!()
    }
}

struct RecordingBackend(Arc<Mutex<Vec<u8>>>);

impl Pl011Backend for RecordingBackend {
    fn transmit(&self, byte: u8) -> Result<(), Pl011BackendError> {
        self.0.lock().unwrap().push(byte);
        Ok(())
    }
}
