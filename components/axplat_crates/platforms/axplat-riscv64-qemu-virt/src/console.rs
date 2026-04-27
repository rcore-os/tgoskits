use core::{hint::spin_loop, ptr::NonNull};

use ax_kspin::SpinNoIrq;
use ax_lazyinit::LazyInit;
use ax_plat::console::ConsoleIf;
#[cfg(feature = "irq")]
use ax_plat::console::ConsoleIrqEvent;
#[cfg(feature = "irq")]
use some_serial::TIrqHandler;
use some_serial::{
    InterfaceRaw, InterruptMask, Reciever as Receiver, Sender, TReciever as TReceiver, TSender,
    ns16550::{Mmio, Ns16550, Ns16550IrqHandler},
};

use crate::config::{devices::UART_PADDR, plat::PHYS_VIRT_OFFSET};

const UART_CLOCK_FREQ: u32 = 1_843_200;
const UART_REG_WIDTH: usize = 1;

struct ConsoleUart {
    _uart: Ns16550<Mmio>,
    tx: Sender,
    rx: Receiver,
}

impl ConsoleUart {
    fn new() -> Self {
        let base = NonNull::new((UART_PADDR + PHYS_VIRT_OFFSET) as *mut u8).unwrap();
        let mut uart = Ns16550::new_mmio(base, UART_CLOCK_FREQ, UART_REG_WIDTH);
        uart.open();
        uart.set_irq_mask(InterruptMask::empty());
        let tx = uart.take_tx().expect("NS16550 TX handle was already taken");
        let rx = uart.take_rx().expect("NS16550 RX handle was already taken");
        let irq_handler = uart
            .irq_handler()
            .expect("NS16550 IRQ handler was already taken");
        UART_IRQ_HANDLER.init_once(irq_handler);
        Self {
            _uart: uart,
            tx,
            rx,
        }
    }

    fn write_byte(&mut self, byte: u8) {
        while !self.tx.write_byte(byte) {
            spin_loop();
        }
    }

    fn read_bytes(&mut self, bytes: &mut [u8]) -> usize {
        match self.rx.read_bytes(bytes) {
            Ok(n) => n,
            Err(err) => err.bytes_transferred,
        }
    }
}

static UART: LazyInit<SpinNoIrq<ConsoleUart>> = LazyInit::new();
static UART_IRQ_HANDLER: LazyInit<Ns16550IrqHandler<Mmio>> = LazyInit::new();

pub(crate) fn init_early() {
    UART.init_once(SpinNoIrq::new(ConsoleUart::new()));
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
                    uart.write_byte(b'\r');
                    uart.write_byte(b'\n');
                }
                c => uart.write_byte(c),
            }
        }
    }

    /// Reads bytes from the console into the given mutable slice.
    /// Returns the number of bytes read.
    fn read_bytes(bytes: &mut [u8]) -> usize {
        let mut uart = UART.lock();
        uart.read_bytes(bytes)
    }

    /// Returns the IRQ number for the console, if applicable.
    #[cfg(feature = "irq")]
    fn irq_num() -> Option<usize> {
        Some(crate::config::devices::UART_IRQ)
    }

    #[cfg(feature = "irq")]
    fn set_input_irq_enabled(enabled: bool) {
        let mut uart = UART.lock();
        let mask = if enabled {
            InterruptMask::RX_AVAILABLE
        } else {
            InterruptMask::empty()
        };
        uart._uart.set_irq_mask(mask);
    }

    #[cfg(feature = "irq")]
    fn handle_irq() -> ConsoleIrqEvent {
        let status = UART_IRQ_HANDLER.clean_interrupt_status();
        if status.contains(InterruptMask::RX_AVAILABLE) {
            ConsoleIrqEvent::RX_READY
        } else {
            ConsoleIrqEvent::empty()
        }
    }
}
