//! Console input and output.

use core::fmt::{Arguments, Result, Write};

use bitflags::bitflags;
pub use rdrive::DeviceId as ConsoleDeviceId;

/// Why the platform could not provide a hardware console device id.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConsoleDeviceIdError {
    /// No firmware or command-line hardware console was specified.
    NotSpecified,
    /// A console was specified, but it does not describe a hardware device.
    NoHardwareDevice,
    /// A hardware console was specified, but no probed device matched it.
    DeviceNotFound,
}

/// Result type returned by the platform console device selector.
pub type ConsoleDeviceIdResult = core::result::Result<ConsoleDeviceId, ConsoleDeviceIdError>;

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

    /// Returns the runtime-discovered hardware device selected as the console.
    ///
    /// Static platforms that do not have a runtime device manager should return
    /// [`ConsoleDeviceIdError::NotSpecified`].
    fn device_id() -> ConsoleDeviceIdResult;

    /// Pauses low-level output before a runtime driver touches the same UART.
    #[doc(hidden)]
    fn begin_runtime_output_handover_raw() -> usize;

    /// Commits a paused low-level output handover.
    #[doc(hidden)]
    fn commit_runtime_output_handover_raw(token: usize) -> bool;

    /// Restores low-level output after a paused handover failed.
    #[doc(hidden)]
    fn abort_runtime_output_handover_raw(token: usize) -> bool;

    /// Returns the IRQ number for the console input interrupt.
    ///
    /// Returns `None` if input interrupt is not supported.
    #[cfg(feature = "irq")]
    fn irq_num() -> Option<irq_framework::IrqId>;

    /// Enables or disables device-side console input interrupts.
    #[cfg(feature = "irq")]
    fn set_input_irq_enabled(enabled: bool);

    /// Handles a console input IRQ in interrupt context and returns the
    /// corresponding device events.
    #[cfg(feature = "irq")]
    fn handle_irq() -> ConsoleIrqEvent;
}

/// Failure to start or complete an exclusive console-hardware handover.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConsoleHandoverError {
    /// Another handover already paused or claimed the low-level console.
    Busy,
    /// The platform rejected a stale or otherwise invalid handover token.
    InvalidToken,
}

impl core::fmt::Display for ConsoleHandoverError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Busy => f.write_str("console hardware handover is already active"),
            Self::InvalidToken => f.write_str("console hardware handover token is invalid"),
        }
    }
}

impl core::error::Error for ConsoleHandoverError {}

/// Exclusive ownership transition from the low-level console to a runtime driver.
///
/// Preparing the token waits for any in-flight early UART access and then makes
/// later low-level reads/writes inert. Dropping an uncommitted token restores
/// the polling console. The platform state and generation are global, so the
/// token may follow a migratable kernel task while runtime startup waits for an
/// owner CPU; it does not represent an IRQ, preemption, or CPU-pin guard.
#[must_use = "dropping the token aborts the runtime console handover"]
pub struct RuntimeOutputHandover {
    token: usize,
    active: bool,
}

impl RuntimeOutputHandover {
    /// Permanently retires low-level output after the runtime UART is ready.
    pub fn commit(mut self) -> core::result::Result<(), ConsoleHandoverError> {
        // A commit attempt is made only after the runtime driver can own the
        // hardware. If the platform rejects the token, restoring early access
        // from Drop could create two register owners. Retire rollback first and
        // leave the low-level path paused for the caller's fatal error path.
        self.active = false;
        if !commit_runtime_output_handover_raw(self.token) {
            return Err(ConsoleHandoverError::InvalidToken);
        }
        Ok(())
    }
}

impl Drop for RuntimeOutputHandover {
    fn drop(&mut self) {
        if self.active {
            let _ = abort_runtime_output_handover_raw(self.token);
        }
    }
}

/// Pauses low-level console access for a fallible runtime UART takeover.
pub fn prepare_runtime_output_handover()
-> core::result::Result<RuntimeOutputHandover, ConsoleHandoverError> {
    let token = begin_runtime_output_handover_raw();
    if token == 0 {
        return Err(ConsoleHandoverError::Busy);
    }
    Ok(RuntimeOutputHandover {
        token,
        active: true,
    })
}

struct EarlyConsole;

impl Write for EarlyConsole {
    fn write_str(&mut self, s: &str) -> Result {
        write_text_bytes(s.as_bytes());
        Ok(())
    }
}

/// Writes text bytes to the console, expanding line feeds to CRLF.
///
/// This is intended for human-readable console output. Use [`write_bytes`] for
/// raw byte transport.
pub fn write_text_bytes(bytes: &[u8]) {
    let mut start = 0;
    for (i, &byte) in bytes.iter().enumerate() {
        if byte == b'\n' {
            if start < i {
                write_bytes(&bytes[start..i]);
            }
            write_bytes(b"\r\n");
            start = i + 1;
        }
    }
    if start < bytes.len() {
        write_bytes(&bytes[start..]);
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

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::impl_plat_interface;

    static ABORT_CALLS: AtomicUsize = AtomicUsize::new(0);

    struct TestConsole;

    #[impl_plat_interface]
    impl ConsoleIf for TestConsole {
        fn write_bytes(_bytes: &[u8]) {}

        fn read_bytes(_bytes: &mut [u8]) -> usize {
            0
        }

        fn device_id() -> ConsoleDeviceIdResult {
            Err(ConsoleDeviceIdError::NotSpecified)
        }

        fn begin_runtime_output_handover_raw() -> usize {
            7
        }

        fn commit_runtime_output_handover_raw(_token: usize) -> bool {
            false
        }

        fn abort_runtime_output_handover_raw(_token: usize) -> bool {
            ABORT_CALLS.fetch_add(1, Ordering::Relaxed);
            true
        }

        #[cfg(feature = "irq")]
        fn irq_num() -> Option<irq_framework::IrqId> {
            None
        }

        #[cfg(feature = "irq")]
        fn set_input_irq_enabled(_enabled: bool) {}

        #[cfg(feature = "irq")]
        fn handle_irq() -> ConsoleIrqEvent {
            ConsoleIrqEvent::empty()
        }
    }

    #[test]
    fn failed_commit_attempt_keeps_the_early_console_suppressed() {
        ABORT_CALLS.store(0, Ordering::Relaxed);
        let handover = prepare_runtime_output_handover().unwrap();

        assert_eq!(handover.commit(), Err(ConsoleHandoverError::InvalidToken));
        assert_eq!(
            ABORT_CALLS.load(Ordering::Relaxed),
            0,
            "a failed commit after runtime startup must not reactivate early UART access"
        );
    }

    #[test]
    fn runtime_handover_can_follow_a_migrating_kernel_task() {
        fn assert_send<T: Send>() {}

        assert_send::<RuntimeOutputHandover>();
    }
}
