use ax_kspin::SpinNoIrq;
use ax_lazyinit::LazyInit;
#[cfg(feature = "irq")]
use ax_plat::console::ConsoleIrqEvent;
use ax_plat::{
    console::{ConsoleDeviceId, ConsoleIf},
    mem::{pa, phys_to_virt},
};
use uart_16550::MmioSerialPort;

use crate::config::devices::UART_PADDR;

static UART: LazyInit<SpinNoIrq<MmioSerialPort>> = LazyInit::new();

pub(crate) fn init_early() {
    UART.init_once({
        let uart =
            unsafe { MmioSerialPort::new_with_stride(phys_to_virt(pa!(UART_PADDR)).as_usize(), 4) };
        // uart.init();
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

    fn device_id() -> Option<ConsoleDeviceId> {
        None
    }

    /// Returns the IRQ number for the console, if applicable.
    #[cfg(feature = "irq")]
    fn irq_num() -> Option<usize> {
        // Some(crate::config::devices::UART_IRQ)
        None
    }

    #[cfg(feature = "irq")]
    fn set_input_irq_enabled(_enabled: bool) {}

    #[cfg(feature = "irq")]
    fn handle_irq() -> ConsoleIrqEvent {
        ConsoleIrqEvent::empty()
    }
}
