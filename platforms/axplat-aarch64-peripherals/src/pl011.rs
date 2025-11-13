//! PL011 UART.

use arm_pl011::Pl011Uart;
use axplat::mem::VirtAddr;
use kspin::SpinNoIrq;
use lazyinit::LazyInit;

static UART: LazyInit<SpinNoIrq<Pl011Uart>> = LazyInit::new();

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
    UART.init_once(SpinNoIrq::new({
        let mut uart = Pl011Uart::new(uart_base.as_mut_ptr());
        uart.init();
        uart
    }));
}

/// UART IRQ Handler
pub fn irq_handler() {
    UART.lock().ack_interrupts();
}

/// Default implementation of [`axplat::console::ConsoleIf`] using the
/// PL011 UART.
#[macro_export]
#[allow(clippy::crate_in_macro_def)]
macro_rules! console_if_impl {
    ($name:ident) => {
        struct $name;

        #[axplat::impl_plat_interface]
        impl axplat::console::ConsoleIf for $name {
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
        }
    };
}
