//! Host-facing character backend for virtual serial ports.

use alloc::sync::Arc;
use core::fmt::Debug;

/// Bidirectional byte stream used by a virtual serial device.
///
/// The backend owns neither UART registers nor interrupt state. Implementations
/// may connect the stream to a terminal multiplexer, a test buffer, or another
/// host service.
pub trait SerialBackend: Send + Sync + Debug {
    /// Writes bytes emitted by the guest.
    fn write(&self, bytes: &[u8]);

    /// Reads host-provided bytes into `buffer` without blocking.
    fn read(&self, buffer: &mut [u8]) -> usize;
}

/// Creates one backend for each virtual UART runtime generation.
///
/// VM resets rebuild the virtual device graph. Returning a fresh backend keeps
/// callbacks retained by an old graph from reaching the replacement runtime.
pub trait SerialBackendFactory: Send + Sync + Debug {
    /// Creates the backend owned by the next virtual UART instance.
    fn create(&self) -> Arc<dyn SerialBackend>;
}

/// Backend used when no terminal service is attached.
#[derive(Debug, Default)]
pub struct NullSerialBackend;

impl SerialBackend for NullSerialBackend {
    fn write(&self, _bytes: &[u8]) {}

    fn read(&self, _buffer: &mut [u8]) -> usize {
        0
    }
}

/// Factory used when no terminal service is attached.
#[derive(Debug, Default)]
pub struct NullSerialBackendFactory;

impl SerialBackendFactory for NullSerialBackendFactory {
    fn create(&self) -> Arc<dyn SerialBackend> {
        Arc::new(NullSerialBackend)
    }
}
