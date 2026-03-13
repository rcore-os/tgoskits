//! Common traits and types for socket communite device drivers (i.e. disk).

#![no_std]
#![cfg_attr(doc, feature(doc_cfg))]

#[doc(no_inline)]
pub use axdriver_base::{BaseDriverOps, DevError, DevResult, DeviceType};

/// Vsock address.
#[derive(Copy, Clone, Debug, Default, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct VsockAddr {
    /// Context Identifier.
    pub cid: u64,
    /// Port number.
    pub port: u32,
}

/// Vsock connection id.
#[derive(Copy, Clone, Debug, Default, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct VsockConnId {
    /// Peer address.
    pub peer_addr: VsockAddr,
    /// Local port.
    pub local_port: u32,
}

impl VsockConnId {
    /// Create a new [`VsockConnId`] for listening socket
    pub fn listening(local_port: u32) -> Self {
        Self {
            peer_addr: VsockAddr { cid: 0, port: 0 },
            local_port,
        }
    }
}

/// VsockDriverEvent
#[derive(Debug)]
pub enum VsockDriverEvent {
    /// ConnectionRequest
    ConnectionRequest(VsockConnId),
    /// Connected
    Connected(VsockConnId),
    /// Received
    Received(VsockConnId, usize),
    /// Disconnected
    Disconnected(VsockConnId),
    /// Credit Update
    CreditUpdate(VsockConnId),
    /// unknown event
    Unknown,
}

/// Operations that require a block storage device driver to implement.
pub trait VsockDriverOps: BaseDriverOps {
    /// guest cid
    fn guest_cid(&self) -> u64;

    /// Listen on a specific port.
    fn listen(&mut self, src_port: u32);

    /// Connect to a peer socket.
    fn connect(&mut self, cid: VsockConnId) -> DevResult<()>;

    /// Send data to the connected peer socket. need addr for DGRAM mode
    fn send(&mut self, cid: VsockConnId, buf: &[u8]) -> DevResult<usize>;

    /// Receive data from the connected peer socket.
    fn recv(&mut self, cid: VsockConnId, buf: &mut [u8]) -> DevResult<usize>;

    /// Returns the number of bytes in the receive buffer available to be read by recv.
    fn recv_avail(&mut self, cid: VsockConnId) -> DevResult<usize>;

    /// Disconnect from the connected peer socket.
    ///
    /// Requests to shut down the connection cleanly, telling the peer that we won't send or receive
    /// any more data.
    fn disconnect(&mut self, cid: VsockConnId) -> DevResult<()>;

    /// Forcibly closes the connection without waiting for the peer.
    fn abort(&mut self, cid: VsockConnId) -> DevResult<()>;

    /// poll event from driver
    fn poll_event(&mut self) -> DevResult<Option<VsockDriverEvent>>;
}
