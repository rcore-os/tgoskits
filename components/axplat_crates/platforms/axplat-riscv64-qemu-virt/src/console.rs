use axplat::console::ConsoleIf;
use kspin::SpinNoIrq;
use lazyinit::LazyInit;
use uart_16550::MmioSerialPort;

use crate::config::{devices::UART_PADDR, plat::PHYS_VIRT_OFFSET};

static UART: LazyInit<SpinNoIrq<MmioSerialPort>> = LazyInit::new();

pub(crate) fn init_early() {
    let base = UART_PADDR + PHYS_VIRT_OFFSET;
    UART.init_once({
        let mut uart = unsafe { MmioSerialPort::new(base) };
        uart.init();
        // `uart_16550` uses non-volatile ptr::write() which the compiler may
        // optimise away.  Re-write the interrupt-critical registers with
        // volatile stores — in particular MCR.OUT2 (bit 3) gates the 16550
        // interrupt output; without it the UART never signals the PLIC.
        unsafe {
            core::ptr::write_volatile((base + 2) as *mut u8, 0x01); // FCR: FIFO enable, 1-byte trigger
            core::ptr::write_volatile((base + 4) as *mut u8, 0x0B); // MCR: DTR+RTS+OUT2
            core::ptr::write_volatile((base + 1) as *mut u8, 0x01); // IER: RX data available
        }
        SpinNoIrq::new(uart)
    });
}

struct ConsoleIfImpl;

#[impl_plat_interface]
impl ConsoleIf for ConsoleIfImpl {
    /// Writes bytes to the console from input u8 slice.
    fn write_bytes(bytes: &[u8]) {
        for &c in bytes {
            let mut uart = UART.lock();
            match c {
                b'\n' => {
                    uart.send_raw(b'\r');
                    uart.send_raw(b'\n');
                }
                c => uart.send_raw(c),
            }
        }
    }

    /// Reads bytes from the console into the given mutable slice.
    /// Returns the number of bytes read.
    fn read_bytes(bytes: &mut [u8]) -> usize {
        let mut uart = UART.lock();
        for (i, byte) in bytes.iter_mut().enumerate() {
            match uart.try_receive() {
                Ok(c) => *byte = c,
                Err(_) => return i,
            }
        }
        bytes.len()
    }

    /// Returns the IRQ number for the console, if applicable.
    #[cfg(feature = "irq")]
    fn irq_num() -> Option<usize> {
        Some(crate::config::devices::UART_IRQ)
    }
}
