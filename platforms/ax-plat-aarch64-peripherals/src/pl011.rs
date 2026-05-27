//! PL011 UART.

use core::{hint::spin_loop, ptr::NonNull};

use ax_kspin::SpinNoIrq;
use ax_lazyinit::LazyInit;
use ax_plat::{console::ConsoleIrqEvent, mem::VirtAddr};
use some_serial::{
    InterfaceRaw, InterruptMask, Reciever as Receiver, Sender, TIrqHandler, TReciever as _,
    TSender as _,
    pl011::{Pl011, Pl011IrqHandler},
};

struct ConsoleUart {
    uart: Pl011,
    tx: Sender,
    rx: Receiver,
}

impl ConsoleUart {
    fn new(uart_base: VirtAddr) -> Self {
        let base = NonNull::new(uart_base.as_mut_ptr()).expect("PL011 MMIO base must be non-null");
        let mut uart = Pl011::new_no_clock(base);
        uart.open();
        uart.set_irq_mask(InterruptMask::empty());
        let tx = uart.take_tx().expect("PL011 TX handle was already taken");
        let rx = uart.take_rx().expect("PL011 RX handle was already taken");
        let irq_handler = uart
            .irq_handler()
            .expect("PL011 IRQ handler was already taken");
        UART_IRQ_HANDLER.init_once(irq_handler);
        Self { uart, tx, rx }
    }

    fn putchar(&mut self, c: u8) {
        while !self.tx.write_byte(c) {
            spin_loop();
        }
    }

    fn getchar(&mut self) -> Option<u8> {
        match self.rx.read_byte() {
            Some(Ok(byte)) => Some(byte),
            Some(Err(_)) | None => None,
        }
    }
}

static UART: LazyInit<SpinNoIrq<ConsoleUart>> = LazyInit::new();
static UART_IRQ_HANDLER: LazyInit<Pl011IrqHandler> = LazyInit::new();

/// Writes a byte to the console.
pub fn putchar(c: u8) {
    UART.lock().putchar(c);
}

/// Reads a byte from the console, or returns [`None`] if no input is available.
pub fn getchar() -> Option<u8> {
    UART.lock().getchar()
}

/// Write a slice of bytes to the console.
pub fn write_bytes(bytes: &[u8]) {
    let mut uart = UART.lock();
    for c in bytes {
        uart.putchar(*c);
    }
}

/// Reads bytes from the console into the given mutable slice.
/// Returns the number of bytes read.
pub fn read_bytes(bytes: &mut [u8]) -> usize {
    let mut read_len = 0;
    while read_len < bytes.len() {
        if let Some(c) = getchar() {
            bytes[read_len] = c;
        } else {
            break;
        }
        read_len += 1;
    }
    read_len
}

/// Early stage initialization of the PL011 UART driver.
pub fn init_early(uart_base: VirtAddr) {
    UART.init_once(SpinNoIrq::new(ConsoleUart::new(uart_base)));
}

/// Enables or disables PL011 receive-side IRQs.
pub fn set_input_irq_enabled(enabled: bool) {
    let mut uart = UART.lock();
    let mask = if enabled {
        InterruptMask::RX_AVAILABLE
    } else {
        InterruptMask::empty()
    };
    uart.uart.set_irq_mask(mask);
}

/// Handles a PL011 input interrupt and returns the corresponding event flags.
pub fn handle_irq() -> ConsoleIrqEvent {
    let status = UART_IRQ_HANDLER.clean_interrupt_status();
    let mut events = ConsoleIrqEvent::empty();
    if status.contains(InterruptMask::RX_AVAILABLE) {
        events |= ConsoleIrqEvent::RX_READY;
    }
    events
}

/// Default implementation of [`ax_plat::console::ConsoleIf`] using the
/// PL011 UART.
#[macro_export]
#[allow(clippy::crate_in_macro_def)]
macro_rules! console_if_impl {
    ($name:ident) => {
        struct $name;

        #[ax_plat::impl_plat_interface]
        impl ax_plat::console::ConsoleIf for $name {
            /// Writes given bytes to the console.
            fn write_bytes(bytes: &[u8]) {
                $crate::pl011::write_bytes(bytes);
            }

            /// Reads bytes from the console into the given mutable slice.
            ///
            /// Returns the number of bytes read.
            fn read_bytes(bytes: &mut [u8]) -> usize {
                $crate::pl011::read_bytes(bytes)
            }

            /// Returns the IRQ number for the console input interrupt.
            ///
            /// Returns `None` if input interrupt is not supported.
            #[cfg(feature = "irq")]
            fn irq_num() -> Option<usize> {
                Some(crate::config::devices::UART_IRQ as _)
            }

            /// Enables or disables device-side console input interrupts.
            #[cfg(feature = "irq")]
            fn set_input_irq_enabled(enabled: bool) {
                $crate::pl011::set_input_irq_enabled(enabled);
            }

            /// Handles a console input IRQ and clears device-side IRQ state.
            #[cfg(feature = "irq")]
            fn handle_irq() -> ax_plat::console::ConsoleIrqEvent {
                $crate::pl011::handle_irq()
            }
        }
    };
}
