use ax_kspin::SpinNoIrq;
use ax_lazyinit::LazyInit;
use ax_plat::console::ConsoleIf;
#[cfg(feature = "irq")]
use ax_plat::console::ConsoleIrqEvent;
use some_serial::ns16550::dw_apb::{DwApbUart, SG2002_UART_CLOCK};

use crate::config::{devices::UART_PADDR, plat::PHYS_VIRT_OFFSET};

static UART: LazyInit<SpinNoIrq<DwApbUart>> = LazyInit::new();

pub(crate) fn init_early() {
    UART.init_once({
        let mut uart = DwApbUart::new(UART_PADDR + PHYS_VIRT_OFFSET);
        // SG2002 uses dw-apb-uart with 25MHz clock, 115200 baud.
        uart.init_with_baud_clk(115_200, SG2002_UART_CLOCK);
        SpinNoIrq::new(uart)
    });
}

struct ConsoleIfImpl;

#[impl_plat_interface]
impl ConsoleIf for ConsoleIfImpl {
    /// Writes bytes to the console from input u8 slice.
    fn write_bytes(bytes: &[u8]) {
        let mut uart = UART.lock();
        for &c in bytes {
            match c {
                b'\n' => {
                    write_byte(&mut uart, b'\r');
                    write_byte(&mut uart, b'\n');
                }
                c => write_byte(&mut uart, c),
            }
        }
    }

    /// Reads bytes from the console into the given mutable slice.
    /// Returns the number of bytes read.
    fn read_bytes(bytes: &mut [u8]) -> usize {
        let mut uart = UART.lock();
        uart.try_read(bytes)
            .unwrap_or_else(|err| err.bytes_transferred)
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

fn write_byte(uart: &mut DwApbUart, byte: u8) {
    while uart.try_write(&[byte]) == 0 {
        core::hint::spin_loop();
    }
}
