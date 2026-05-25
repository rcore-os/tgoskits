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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event {
    pub connection_changed: bool,
    pub data_available: bool,
}

impl Event {
    pub const fn none() -> Self {
        Self {
            connection_changed: false,
            data_available: false,
        }
    }
}
