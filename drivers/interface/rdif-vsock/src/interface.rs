use crate::{DriverGeneric, Event, VsockConnId, VsockError, VsockEvent};

pub trait Interface: DriverGeneric {
    fn guest_cid(&self) -> u64;

    fn listen(&mut self, port: u32) -> Result<(), VsockError>;

    fn connect(&mut self, id: VsockConnId) -> Result<(), VsockError>;

    fn send(&mut self, id: VsockConnId, buf: &[u8]) -> Result<usize, VsockError>;

    fn recv(&mut self, id: VsockConnId, buf: &mut [u8]) -> Result<usize, VsockError>;

    fn recv_avail(&mut self, id: VsockConnId) -> Result<usize, VsockError>;

    fn disconnect(&mut self, id: VsockConnId) -> Result<(), VsockError>;

    fn abort(&mut self, id: VsockConnId) -> Result<(), VsockError>;

    fn poll_event(&mut self) -> Result<Option<VsockEvent>, VsockError>;

    fn enable_irq(&mut self) {}

    fn disable_irq(&mut self) {}

    fn is_irq_enabled(&self) -> bool {
        false
    }

    fn handle_irq(&mut self) -> Event {
        Event::none()
    }
}
