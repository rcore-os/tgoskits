use crate::{DriverGeneric, VsockConnId, VsockError, VsockEvent, VsockIrqEndpoints};

pub trait Interface: DriverGeneric {
    fn guest_cid(&self) -> u64;

    fn listen(&mut self, port: u32) -> Result<(), VsockError>;

    fn connect(&mut self, id: VsockConnId) -> Result<(), VsockError>;

    /// Returns the bytes that one send may publish without waiting for peer
    /// credit.
    ///
    /// The value is a task-context transport snapshot. Implementations must
    /// account for protocol flow control and must return zero rather than an
    /// optimistic capacity when the peer window is exhausted.
    fn send_capacity(&mut self, id: VsockConnId) -> Result<usize, VsockError>;

    fn send(&mut self, id: VsockConnId, buf: &[u8]) -> Result<usize, VsockError>;

    fn recv(&mut self, id: VsockConnId, buf: &mut [u8]) -> Result<usize, VsockError>;

    fn recv_avail(&mut self, id: VsockConnId) -> Result<usize, VsockError>;

    fn disconnect(&mut self, id: VsockConnId) -> Result<(), VsockError>;

    fn abort(&mut self, id: VsockConnId) -> Result<(), VsockError>;

    fn poll_event(&mut self) -> Result<Option<VsockEvent>, VsockError>;

    /// Transfers the device's mandatory IRQ capabilities to the runtime.
    ///
    /// A second call must return [`VsockError::NotAvailable`].
    fn take_irq_endpoints(&mut self) -> Result<VsockIrqEndpoints, VsockError>;
}
