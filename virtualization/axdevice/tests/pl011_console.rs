use std::sync::Mutex;

use axdevice::{AccessWidth, Device, GuestPhysAddr, Pl011ConsoleDevice, Pl011ConsoleHostOps};
use axdevice_base::{BusAccess, BusKind, BusResponse, DeviceError, Resource};

const UART_BASE: usize = 0x0900_0000;
const UART_SIZE: usize = 0x1000;

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

static OUTPUT: Mutex<Vec<(String, Vec<u8>)>> = Mutex::new(Vec::new());

struct MockHost;

impl Pl011ConsoleHostOps for MockHost {
    fn write_console_chunk(console: &str, bytes: &[u8]) {
        OUTPUT
            .lock()
            .unwrap()
            .push((console.to_owned(), bytes.to_vec()));
    }
}

fn new_console() -> Pl011ConsoleDevice<MockHost> {
    OUTPUT.lock().unwrap().clear();
    Pl011ConsoleDevice::new(
        "linux-controller".into(),
        GuestPhysAddr::from_usize(UART_BASE),
        UART_SIZE,
    )
    .unwrap()
}

fn read(device: &Pl011ConsoleDevice<MockHost>, offset: usize) -> Result<u64, DeviceError> {
    match device.handle(&BusAccess {
        kind: BusKind::Mmio,
        is_read: true,
        addr: (UART_BASE + offset) as u64,
        width: AccessWidth::Dword,
        data: 0,
    })? {
        BusResponse::Read { value } => Ok(value),
        BusResponse::Write => panic!("MMIO read returned a write response"),
    }
}

fn write(
    device: &Pl011ConsoleDevice<MockHost>,
    offset: usize,
    value: u32,
) -> Result<(), DeviceError> {
    write_with_width(device, offset, AccessWidth::Dword, value)
}

fn write_with_width(
    device: &Pl011ConsoleDevice<MockHost>,
    offset: usize,
    width: AccessWidth,
    value: u32,
) -> Result<(), DeviceError> {
    match device.handle(&BusAccess {
        kind: BusKind::Mmio,
        is_read: false,
        addr: (UART_BASE + offset) as u64,
        width,
        data: value as u64,
    })? {
        BusResponse::Write => Ok(()),
        BusResponse::Read { .. } => panic!("MMIO write returned a read response"),
    }
}

#[test]
fn exposes_a_pl011_mmio_window_and_polled_transmit_state() {
    let device = new_console();

    assert_eq!(
        device.resources(),
        &[Resource::MmioRange {
            base: UART_BASE as u64,
            size: UART_SIZE as u64,
        }]
    );
    assert_eq!(read(&device, UARTFR).unwrap(), UARTFR_RXFE | UARTFR_TXFE);
    assert_eq!(read(&device, UARTMIS).unwrap(), 0);
    assert_eq!(read(&device, UARTPID0).unwrap(), 0x11);
    assert_eq!(read(&device, UARTCID3).unwrap(), 0xb1);
}

#[test]
fn preserves_driver_configuration_and_forwards_complete_lines() {
    let device = new_console();

    for (register, value) in [
        (UARTCR, 0),
        (UARTIBRD, 13),
        (UARTFBRD, 1),
        (UARTLCR_H, 0x70),
        (UARTIMSC, 0),
        (UARTCR, 0x301),
    ] {
        write(&device, register, value).unwrap();
        assert_eq!(read(&device, register).unwrap(), value as u64);
    }

    for byte in b"IVC-RTOS-READY\r\n" {
        write(&device, UARTDR, u32::from(*byte)).unwrap();
    }

    assert_eq!(
        *OUTPUT.lock().unwrap(),
        vec![(
            "linux-controller".to_owned(),
            b"IVC-RTOS-READY\r\n".to_vec()
        )]
    );
}

#[test]
fn rejects_ranges_that_cannot_expose_the_primecell_identification_registers() {
    let result = Pl011ConsoleDevice::<MockHost>::new(
        "short-uart".into(),
        GuestPhysAddr::from_usize(UART_BASE),
        UART_SIZE - 1,
    );

    assert!(matches!(result, Err(DeviceError::InvalidInput { .. })));
}

#[test]
fn forwards_linux_style_byte_writes_to_the_data_register() {
    let device = new_console();

    for byte in b"Linux console\n" {
        write_with_width(&device, UARTDR, AccessWidth::Byte, u32::from(*byte)).unwrap();
    }

    assert_eq!(
        *OUTPUT.lock().unwrap(),
        vec![("linux-controller".to_owned(), b"Linux console\n".to_vec())]
    );
}

#[test]
fn keeps_a_long_result_line_in_one_host_chunk() {
    let device = new_console();
    let mut result_line = vec![b'x'; 512];
    result_line.push(b'\n');

    for byte in &result_line {
        write_with_width(&device, UARTDR, AccessWidth::Byte, u32::from(*byte)).unwrap();
    }

    assert_eq!(
        *OUTPUT.lock().unwrap(),
        vec![("linux-controller".to_owned(), result_line)]
    );
}

#[test]
fn rejects_qword_and_out_of_range_accesses() {
    let device = new_console();
    let qword_access = BusAccess {
        kind: BusKind::Mmio,
        is_read: true,
        addr: (UART_BASE + UARTFR) as u64,
        width: AccessWidth::Qword,
        data: 0,
    };
    let outside_access = BusAccess {
        kind: BusKind::Mmio,
        is_read: true,
        addr: (UART_BASE + UART_SIZE) as u64,
        width: AccessWidth::Dword,
        data: 0,
    };

    assert!(matches!(
        device.handle(&qword_access),
        Err(DeviceError::InvalidWidth { .. })
    ));
    assert!(matches!(
        device.handle(&outside_access),
        Err(DeviceError::OutOfRange { .. })
    ));
}
