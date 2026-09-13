use crate::VsockConnId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VsockEvent {
    ConnectionRequest(VsockConnId),
    Connected(VsockConnId),
    Received(VsockConnId, usize),
    Disconnected(VsockConnId),
    CreditUpdate(VsockConnId),
    Unknown,
}
