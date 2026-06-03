//! Console APIs for virtual console devices.

use ax_errno::AxResult;

/// A byte-input adapter backed by the host console.
#[derive(Default)]
pub struct ConsoleReader;

impl ConsoleReader {
    /// Creates a new console reader.
    pub const fn new() -> Self {
        Self
    }

    /// Reads bytes from the host console.
    pub fn read(&mut self, bytes: &mut [u8]) -> AxResult<usize> {
        Ok(read_bytes(bytes))
    }
}

/// A formatted-output adapter backed by the host console.
#[derive(Default)]
pub struct ConsoleWriter;

impl ConsoleWriter {
    /// Creates a new console writer.
    pub const fn new() -> Self {
        Self
    }

    /// Writes all bytes to the host console.
    pub fn write_all(&mut self, bytes: &[u8]) -> AxResult<()> {
        write_bytes(bytes);
        Ok(())
    }

    /// Flushes pending console output.
    pub fn flush(&mut self) -> AxResult<()> {
        Ok(())
    }
}

impl core::fmt::Write for ConsoleWriter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        write_bytes(s.as_bytes());
        Ok(())
    }
}

/// Console I/O interface exposed by the hypervisor host.
#[crate::api_def]
pub trait ConsoleIf {
    /// Writes raw bytes to the host console.
    fn write_bytes(bytes: &[u8]);

    /// Reads bytes from the host console into `bytes`.
    ///
    /// Returns the number of bytes read.
    fn read_bytes(bytes: &mut [u8]) -> usize;
}
