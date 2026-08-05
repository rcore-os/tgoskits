use std::sync::{Arc, Barrier, Mutex};

use axdevice::{AccessWidth, Pl011, SerialBackend};
use axdevice_base::{
    ControllerInputId, DeviceError, InterruptControllerId, InterruptTriggerMode, IrqResult,
    WiredIrqInput, WiredIrqSink,
};

const UARTDR: usize = 0x000;
const UARTFR: usize = 0x018;
const UARTIBRD: usize = 0x024;
const UARTFBRD: usize = 0x028;
const UARTLCR_H: usize = 0x02c;
const UARTCR: usize = 0x030;
const UARTIMSC: usize = 0x038;
const UARTMIS: usize = 0x040;
const UARTPID0: usize = 0xfe0;
const UARTCID3: usize = 0xffc;

const UARTFR_RXFE: u64 = 1 << 4;
const UARTFR_TXFE: u64 = 1 << 7;

#[derive(Debug, Default)]
struct RecordingBackend {
    writes: Mutex<Vec<Vec<u8>>>,
}

impl RecordingBackend {
    fn output(&self) -> Vec<u8> {
        self.writes.lock().unwrap().concat()
    }
}

impl SerialBackend for RecordingBackend {
    fn write(&self, bytes: &[u8]) {
        self.writes.lock().unwrap().push(bytes.to_vec());
    }

    fn read(&self, _buffer: &mut [u8]) -> usize {
        0
    }
}

#[derive(Debug, Default)]
struct RecordingIrqSink {
    levels: Mutex<Vec<bool>>,
}

impl WiredIrqSink for RecordingIrqSink {
    fn set_level(&self, _input: ControllerInputId, asserted: bool) -> IrqResult {
        self.levels.lock().unwrap().push(asserted);
        Ok(())
    }

    fn pulse(&self, _input: ControllerInputId) -> IrqResult {
        Ok(())
    }
}

fn new_console() -> (Pl011, Arc<RecordingBackend>, Arc<RecordingIrqSink>) {
    let backend = Arc::new(RecordingBackend::default());
    let irq_sink = Arc::new(RecordingIrqSink::default());
    let irq = WiredIrqInput::new(
        InterruptControllerId::new(0),
        ControllerInputId::new(33),
        InterruptTriggerMode::LevelTriggered,
        irq_sink.clone(),
    )
    .connect()
    .unwrap();
    (Pl011::new(backend.clone(), irq), backend, irq_sink)
}

fn write(device: &Pl011, offset: usize, value: u32) -> Result<(), DeviceError> {
    write_with_width(device, offset, AccessWidth::Dword, value)
}

fn write_with_width(
    device: &Pl011,
    offset: usize,
    width: AccessWidth,
    value: u32,
) -> Result<(), DeviceError> {
    device.write(offset, width, u64::from(value))
}

#[test]
fn exposes_primecell_identification_and_polled_transmit_state() {
    let (device, ..) = new_console();

    let flags = device.read(UARTFR, AccessWidth::Dword).unwrap();
    assert_eq!(
        flags & (UARTFR_RXFE | UARTFR_TXFE),
        UARTFR_RXFE | UARTFR_TXFE
    );
    assert_eq!(device.read(UARTMIS, AccessWidth::Dword).unwrap(), 0);
    assert_eq!(device.read(UARTPID0, AccessWidth::Dword).unwrap(), 0x11);
    assert_eq!(device.read(UARTCID3, AccessWidth::Dword).unwrap(), 0xb1);
}

#[test]
fn preserves_driver_configuration_and_forwards_guest_output() {
    let (device, backend, _) = new_console();

    for (register, value) in [
        (UARTCR, 0),
        (UARTIBRD, 13),
        (UARTFBRD, 1),
        (UARTLCR_H, 0x70),
        (UARTIMSC, 0),
        (UARTCR, 0x301),
    ] {
        write(&device, register, value).unwrap();
        assert_eq!(
            device.read(register, AccessWidth::Dword).unwrap(),
            u64::from(value)
        );
    }

    for byte in b"IVC-RTOS-READY\r\n" {
        write(&device, UARTDR, u32::from(*byte)).unwrap();
    }

    assert_eq!(backend.output(), b"IVC-RTOS-READY\r\n");
}

#[test]
fn forwards_linux_style_byte_writes_to_the_data_register() {
    let (device, backend, _) = new_console();

    for byte in b"Linux console\n" {
        write_with_width(&device, UARTDR, AccessWidth::Byte, u32::from(*byte)).unwrap();
    }

    assert_eq!(backend.output(), b"Linux console\n");
}

#[test]
fn preserves_a_long_result_without_truncation() {
    let (device, backend, _) = new_console();
    let mut result_line = vec![b'x'; 512];
    result_line.push(b'\n');

    for byte in &result_line {
        write_with_width(&device, UARTDR, AccessWidth::Byte, u32::from(*byte)).unwrap();
    }

    assert_eq!(backend.output(), result_line);
}

#[test]
fn concurrent_consoles_keep_their_backends_isolated() {
    let (first_device, first_backend, _) = new_console();
    let (second_device, second_backend, _) = new_console();
    let barrier = Arc::new(Barrier::new(2));

    let first_barrier = barrier.clone();
    let first = std::thread::spawn(move || {
        first_barrier.wait();
        for byte in b"first\n" {
            write_with_width(&first_device, UARTDR, AccessWidth::Byte, u32::from(*byte)).unwrap();
        }
    });
    let second = std::thread::spawn(move || {
        barrier.wait();
        for byte in b"second\n" {
            write_with_width(&second_device, UARTDR, AccessWidth::Byte, u32::from(*byte)).unwrap();
        }
    });

    first.join().unwrap();
    second.join().unwrap();
    assert_eq!(first_backend.output(), b"first\n");
    assert_eq!(second_backend.output(), b"second\n");
}

#[test]
fn rejects_qword_and_out_of_range_accesses() {
    let (device, ..) = new_console();

    assert!(matches!(
        device.read(UARTFR, AccessWidth::Qword),
        Err(DeviceError::InvalidWidth { .. })
    ));
    assert!(matches!(
        device.read(0x1000, AccessWidth::Dword),
        Err(DeviceError::OutOfRange { .. })
    ));
}
