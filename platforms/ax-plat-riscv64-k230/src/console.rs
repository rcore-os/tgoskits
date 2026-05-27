use ax_kspin::SpinNoIrq;
use ax_lazyinit::LazyInit;
use ax_plat::console::ConsoleIf;
#[cfg(feature = "irq")]
use ax_plat::console::ConsoleIrqEvent;
use some_serial::ns16550::dw_apb::DwApbUart;

use crate::config::{
    devices::{UART_CLOCK, UART_PADDR},
    plat::PHYS_VIRT_OFFSET,
};

static UART: LazyInit<SpinNoIrq<DwApbUart>> = LazyInit::new();

pub(crate) fn init_early() {
    UART.init_once({
        let mut uart = DwApbUart::new(UART_PADDR + PHYS_VIRT_OFFSET);
        uart.init_with_baud_clk(115_200, UART_CLOCK as u32);
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
                    uart.putchar(b'\r');
                    uart.putchar(b'\n');
                }
                c => uart.putchar(c),
            }
        }
    }

    /// Reads bytes from the console into the given mutable slice.
    /// Returns the number of bytes read.
    fn read_bytes(bytes: &mut [u8]) -> usize {
        let mut uart = UART.lock();
        for (i, byte) in bytes.iter_mut().enumerate() {
            match uart.getchar() {
                Some(c) => *byte = c,
                None => return i,
            }
        }
        bytes.len()
    }

    /// Returns the IRQ number for the console, if applicable.
    #[cfg(feature = "irq")]
    fn irq_num() -> Option<usize> {
        None
    }

    #[cfg(feature = "irq")]
    fn set_input_irq_enabled(_enabled: bool) {}

    #[cfg(feature = "irq")]
    fn handle_irq() -> ConsoleIrqEvent {
        ConsoleIrqEvent::empty()
    }
}
