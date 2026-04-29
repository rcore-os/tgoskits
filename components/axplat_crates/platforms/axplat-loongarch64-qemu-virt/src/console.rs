use ax_kspin::SpinNoIrq;
use ax_lazyinit::LazyInit;
use ax_plat::console::ConsoleIf;
#[cfg(feature = "irq")]
use ax_plat::console::ConsoleIrqEvent;
#[cfg(feature = "irq")]
use uart_16550::spec::registers::InterruptType;
use uart_16550::{Config, Uart16550, backend::MmioBackend, spec::registers::IER};

use crate::config::{devices::UART_PADDR, plat::PHYS_VIRT_OFFSET};

static UART: LazyInit<SpinNoIrq<Uart16550<MmioBackend>>> = LazyInit::new();

fn uart_config(input_irq_enabled: bool) -> Config {
    Config {
        interrupts: if input_irq_enabled {
            IER::DATA_READY | IER::RECEIVER_LINE_STATUS
        } else {
            IER::empty()
        },
        ..Default::default()
    }
}

pub(crate) fn init_early() {
    UART.init_once({
        let mut uart =
            unsafe { Uart16550::new_mmio((UART_PADDR + PHYS_VIRT_OFFSET) as *mut u8, 1) }.unwrap();
        uart.init(uart_config(false))
            .expect("Failed to initialize UART");
        uart.test_loopback().expect("Failed to test UART loopback");
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
                b'\n' => uart.send_bytes_exact(b"\r\n"),
                c => uart.send_bytes_exact(&[c]),
            }
        }
    }

    /// Reads bytes from the console into the given mutable slice.
    /// Returns the number of bytes read.
    fn read_bytes(bytes: &mut [u8]) -> usize {
        let mut uart = UART.lock();
        uart.try_receive_bytes(bytes)
    }

    /// Returns the IRQ number for the console, if applicable.
    #[cfg(feature = "irq")]
    fn irq_num() -> Option<usize> {
        Some(crate::config::devices::UART_IRQ)
    }

    #[cfg(feature = "irq")]
    fn set_input_irq_enabled(enabled: bool) {
        let mut uart = UART.lock();
        uart.init(uart_config(enabled))
            .expect("Failed to configure UART input IRQ");
    }

    #[cfg(feature = "irq")]
    fn handle_irq() -> ConsoleIrqEvent {
        let interrupt = UART.lock().isr().interrupt_type();
        match interrupt {
            Some(InterruptType::ReceiverLineStatus) => ConsoleIrqEvent::RX_ERROR,
            Some(InterruptType::ReceivedDataReady | InterruptType::ReceptionTimeout) => {
                ConsoleIrqEvent::RX_READY
            }
            _ => ConsoleIrqEvent::empty(),
        }
    }
}
