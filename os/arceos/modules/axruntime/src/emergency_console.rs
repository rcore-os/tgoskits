//! Synchronous emergency text output.
//!
//! This is the only public console path for panic, oops, and other contexts
//! that cannot sleep. It performs a direct, bounded hardware attempt and never
//! waits for the serial worker or acquires a sleepable lock. When runtime
//! ownership has not been committed yet, it uses the platform early console.

use core::fmt::{self, Write};

/// Synchronously attempts to write one formatted emergency record.
///
/// The call itself does not queue work, allocate, or sleep. A contended or
/// stalled UART may cause a partial write so panic handling cannot deadlock on
/// an interrupted register owner or broken hardware.
pub fn write_fmt(args: fmt::Arguments<'_>) -> usize {
    if let Some(written) = crate::serial::emergency_write(args) {
        return written;
    }

    let mut writer = PlatformEmergencyWriter::default();
    let _ = fmt::write(&mut writer, args);
    writer.written
}

#[derive(Default)]
struct PlatformEmergencyWriter {
    written: usize,
}

impl Write for PlatformEmergencyWriter {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        crate::hal::console::write_text_bytes(text.as_bytes());
        self.written = self.written.saturating_add(text.len());
        Ok(())
    }
}
