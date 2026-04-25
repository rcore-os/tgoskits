//! Console input and output.

use core::fmt::{Arguments, Result, Write};

use bitflags::bitflags;

bitflags! {
    /// Console input IRQ events returned by the platform.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct ConsoleIrqEvent: u32 {
        /// Console input is ready to be drained.
        const RX_READY = 1 << 0;
        /// A receive-side error was reported.
        const RX_ERROR = 1 << 1;
        /// An overrun was reported.
        const OVERRUN = 1 << 2;
        /// The IRQ was not relevant to console input.
        const SPURIOUS = 1 << 31;
    }
}

/// Console input and output interface.
#[def_plat_interface]
pub trait ConsoleIf {
    /// Writes given bytes to the console.
    fn write_bytes(bytes: &[u8]);

    /// Reads bytes from the console into the given mutable slice.
    ///
    /// Returns the number of bytes read.
    fn read_bytes(bytes: &mut [u8]) -> usize;

    /// Returns the IRQ number for the console input interrupt.
    ///
    /// Returns `None` if input interrupt is not supported.
    #[cfg(feature = "irq")]
    fn irq_num() -> Option<usize>;

    /// Enables or disables device-side console input interrupts.
    #[cfg(feature = "irq")]
    fn set_input_irq_enabled(enabled: bool);

    /// Handles a console input IRQ in interrupt context and returns the
    /// corresponding device events.
    #[cfg(feature = "irq")]
    fn handle_irq() -> ConsoleIrqEvent;
}

struct EarlyConsole;

impl Write for EarlyConsole {
    fn write_str(&mut self, s: &str) -> Result {
        write_bytes(s.as_bytes());
        Ok(())
    }
}

/// Lock for console operations to prevent mixed output from concurrent execution
pub static CONSOLE_LOCK: ax_kspin::SpinNoIrq<()> = ax_kspin::SpinNoIrq::new(());

/// Simple console print operation.
#[macro_export]
macro_rules! console_print {
    ($($arg:tt)*) => {
        $crate::console::__simple_print(format_args!($($arg)*));
    }
}

/// Simple console print operation, with a newline.
#[macro_export]
macro_rules! console_println {
    () => { $crate::ax_print!("\n") };
    ($($arg:tt)*) => {
        $crate::console::__simple_print(format_args!("{}\n", format_args!($($arg)*)));
    }
}

#[doc(hidden)]
pub fn __simple_print(fmt: Arguments) {
    let _guard = CONSOLE_LOCK.lock();
    EarlyConsole.write_fmt(fmt).unwrap();
    drop(_guard);
}
