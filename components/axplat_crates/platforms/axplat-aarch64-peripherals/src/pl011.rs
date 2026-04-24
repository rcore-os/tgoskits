//! PL011 UART.

use core::ptr::{read_volatile, write_volatile};

use ax_arm_pl011::Pl011Uart;
use ax_kspin::SpinNoIrq;
use ax_lazyinit::LazyInit;
use ax_plat::{console::ConsoleIrqEvent, mem::VirtAddr};

static UART: LazyInit<SpinNoIrq<Pl011Uart>> = LazyInit::new();
static UART_BASE: LazyInit<usize> = LazyInit::new();

const PL011_IMSC: usize = 0x38;
const PL011_MIS: usize = 0x40;
const PL011_ICR: usize = 0x44;

const PL011_RX_INT: u32 = 1 << 4;
const PL011_RT_INT: u32 = 1 << 6;
const PL011_FE_INT: u32 = 1 << 7;
const PL011_PE_INT: u32 = 1 << 8;
const PL011_BE_INT: u32 = 1 << 9;
const PL011_OE_INT: u32 = 1 << 10;
const PL011_INPUT_IRQ_MASK: u32 =
    PL011_RX_INT | PL011_RT_INT | PL011_FE_INT | PL011_PE_INT | PL011_BE_INT | PL011_OE_INT;

fn read_reg(offset: usize) -> u32 {
    unsafe { read_volatile(((*UART_BASE) + offset) as *const u32) }
}

fn write_reg(offset: usize, value: u32) {
    unsafe { write_volatile(((*UART_BASE) + offset) as *mut u32, value) }
}

fn do_putchar(uart: &mut Pl011Uart, c: u8) {
    match c {
        b'\n' => {
            uart.putchar(b'\r');
            uart.putchar(b'\n');
        }
        c => uart.putchar(c),
    }
}

/// Writes a byte to the console.
pub fn putchar(c: u8) {
    do_putchar(&mut UART.lock(), c);
}

/// Reads a byte from the console, or returns [`None`] if no input is available.
pub fn getchar() -> Option<u8> {
    UART.lock().getchar()
}

/// Write a slice of bytes to the console.
pub fn write_bytes(bytes: &[u8]) {
    let mut uart = UART.lock();
    for c in bytes {
        do_putchar(&mut uart, *c);
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
    UART_BASE.init_once(uart_base.as_usize());
    UART.init_once(SpinNoIrq::new({
        let mut uart = Pl011Uart::new(uart_base.as_mut_ptr());
        uart.init();
        uart
    }));
    set_input_irq_enabled(false);
}

/// Enables or disables PL011 receive-side IRQs.
pub fn set_input_irq_enabled(enabled: bool) {
    let _guard = UART.lock();
    let imsc = read_reg(PL011_IMSC);
    let imsc = if enabled {
        imsc | PL011_INPUT_IRQ_MASK
    } else {
        imsc & !PL011_INPUT_IRQ_MASK
    };
    write_reg(PL011_IMSC, imsc);
}

/// Handles a PL011 input interrupt and returns the corresponding event flags.
pub fn handle_irq() -> ConsoleIrqEvent {
    let _guard = UART.lock();
    let mis = read_reg(PL011_MIS);
    let clear = mis & PL011_INPUT_IRQ_MASK;
    if clear != 0 {
        write_reg(PL011_ICR, clear);
    }

    let mut events = ConsoleIrqEvent::empty();
    if mis & (PL011_RX_INT | PL011_RT_INT) != 0 {
        events |= ConsoleIrqEvent::RX_READY;
    }
    if mis & PL011_OE_INT != 0 {
        events |= ConsoleIrqEvent::OVERRUN;
    }
    if mis & (PL011_FE_INT | PL011_PE_INT | PL011_BE_INT | PL011_OE_INT) != 0 {
        events |= ConsoleIrqEvent::RX_ERROR;
    }

    if events.is_empty() {
        ConsoleIrqEvent::SPURIOUS
    } else {
        events
    }
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
