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

/// Error returned by an invalid early/runtime console ownership transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ConsoleHandoffError {
    /// The platform console is not in the state required by this transition.
    #[error("invalid console ownership transition")]
    InvalidState,
}

/// Result returned by console ownership transitions.
pub type ConsoleHandoffResult = core::result::Result<(), ConsoleHandoffError>;

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

    /// Stops new low-level console accesses and drains in-flight accesses.
    fn begin_runtime_handoff() -> ConsoleHandoffResult;

    /// Publishes runtime ownership after configuration and routing succeed.
    fn commit_runtime_handoff() -> ConsoleHandoffResult;

    /// Restores low-level ownership after a recoverable preparation failure.
    fn rollback_runtime_handoff() -> ConsoleHandoffResult;

    /// Permanently prevents low-level access after an uncertain failure.
    fn fail_runtime_handoff_closed();

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
pub static CONSOLE_LOCK: ax_sync::SpinLock<()> = ax_sync::SpinLock::new(());

const PENDING_CONSOLE_LINE_CAPACITY: usize = 4096;

struct PendingConsoleLine {
    bytes: [u8; PENDING_CONSOLE_LINE_CAPACITY],
    start: usize,
    len: usize,
}

impl PendingConsoleLine {
    const fn new() -> Self {
        Self {
            bytes: [0; PENDING_CONSOLE_LINE_CAPACITY],
            start: 0,
            len: 0,
        }
    }

    fn observe(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if byte == b'\n' {
                self.start = 0;
                self.len = 0;
                continue;
            }
            let index = (self.start + self.len) % PENDING_CONSOLE_LINE_CAPACITY;
            self.bytes[index] = byte;
            if self.len == PENDING_CONSOLE_LINE_CAPACITY {
                self.start = (self.start + 1) % PENDING_CONSOLE_LINE_CAPACITY;
            } else {
                self.len += 1;
            }
        }
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn replay(&self) {
        let first_len = self
            .len
            .min(PENDING_CONSOLE_LINE_CAPACITY.saturating_sub(self.start));
        write_bytes(&self.bytes[self.start..self.start + first_len]);
        write_bytes(&self.bytes[..self.len - first_len]);
    }

    #[cfg(test)]
    fn to_vec(&self) -> std::vec::Vec<u8> {
        let first_len = self
            .len
            .min(PENDING_CONSOLE_LINE_CAPACITY.saturating_sub(self.start));
        self.bytes[self.start..self.start + first_len]
            .iter()
            .chain(self.bytes[..self.len - first_len].iter())
            .copied()
            .collect()
    }
}

static PENDING_CONSOLE_LINE: ax_sync::SpinLock<PendingConsoleLine> =
    ax_sync::SpinLock::new(PendingConsoleLine::new());

fn with_console_output_lock<T>(operation: impl FnOnce() -> T) -> T {
    let _guard = CONSOLE_LOCK.lock_irqsave();
    operation()
}

/// Writes one already-formatted output segment without allowing other console records to split it.
pub fn write_serialized_bytes(bytes: &[u8]) {
    with_console_output_lock(|| {
        write_bytes(bytes);
        PENDING_CONSOLE_LINE.lock().observe(bytes);
    });
}

/// Formats one console record while holding the output lock for every fragment.
pub fn write_text_fmt(fmt: Arguments<'_>) -> Result {
    with_console_output_lock(|| {
        let pending = PENDING_CONSOLE_LINE.lock();
        if !pending.is_empty() {
            write_bytes(b"\r\n");
        }

        let result = EarlyConsole.write_fmt(fmt);
        if !pending.is_empty() {
            write_bytes(b"\r\n");
            pending.replay();
        }
        result
    })
}

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
    write_text_fmt(fmt).unwrap();
}

#[cfg(test)]
mod tests {
    use core::{
        panic::Location,
        sync::atomic::{AtomicBool, Ordering},
    };
    use std::sync::mpsc;

    use ax_sync::interface::{AcquireResult, ContextOps, LockMetadata, SpinOps};

    use super::{CONSOLE_LOCK, PendingConsoleLine, with_console_output_lock};
    use crate::impl_plat_interface;

    struct TestContextOps;

    #[impl_plat_interface]
    impl ContextOps for TestContextOps {
        fn enter(_context: u8) -> usize {
            0
        }

        fn exit(_context: u8, _state: usize) {}
    }

    struct TestSpinOps;

    #[impl_plat_interface]
    impl SpinOps for TestSpinOps {
        fn acquire(
            locked: &AtomicBool,
            _metadata: &LockMetadata,
            _lock_addr: usize,
            _context: u8,
            _subclass: u32,
            is_try: bool,
            _caller: &'static Location<'static>,
        ) -> AcquireResult {
            loop {
                if locked
                    .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
                {
                    return AcquireResult::new(true, 0);
                }
                if is_try {
                    return AcquireResult::new(false, 0);
                }
                core::hint::spin_loop();
            }
        }

        fn release(locked: &AtomicBool, _lock_addr: usize, _context: u8, _context_state: usize) {
            locked.store(false, Ordering::Release);
        }

        fn force_release(locked: &AtomicBool, _lock_addr: usize, _context: u8) {
            locked.store(false, Ordering::Release);
        }

        fn is_locked(locked: &AtomicBool) -> bool {
            locked.load(Ordering::Relaxed)
        }
    }

    #[test]
    fn serialized_console_output_holds_the_shared_lock_for_the_whole_operation() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (checked_tx, checked_rx) = mpsc::channel();
        let contender = std::thread::spawn(move || {
            entered_rx.recv().unwrap();
            checked_tx.send(CONSOLE_LOCK.try_lock().is_none()).unwrap();
        });

        with_console_output_lock(|| {
            entered_tx.send(()).unwrap();
            assert!(
                checked_rx.recv().unwrap(),
                "a competing host log must not split serialized guest output"
            );
        });
        contender.join().unwrap();
    }

    #[test]
    fn pending_console_line_replays_the_current_guest_fragment() {
        let mut line = PendingConsoleLine::new();
        line.observe(b"l");
        line.observe(b"inux ivc demo pass");

        assert_eq!(line.to_vec(), b"linux ivc demo pass");

        line.observe(b"\r\n");
        assert!(line.to_vec().is_empty());
    }

    #[test]
    fn pending_console_line_keeps_a_bounded_suffix() {
        let mut line = PendingConsoleLine::new();
        let oversized = vec![b'x'; super::PENDING_CONSOLE_LINE_CAPACITY + 3];
        line.observe(&oversized);
        line.observe(b"END");

        let replay = line.to_vec();
        assert_eq!(replay.len(), super::PENDING_CONSOLE_LINE_CAPACITY);
        assert!(replay.ends_with(b"END"));
    }
}
