//! Common traits and types for vsock (virtio socket) device drivers.

#![no_std]
#![cfg_attr(doc, feature(doc_cfg))]

#[doc(no_inline)]
pub use ax_driver_base::{BaseDriverOps, DevError, DevResult, DeviceType};

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

/// Operations that require a vsock device driver to implement.
pub trait VsockDriverOps: BaseDriverOps {
    /// Returns the guest CID.
    fn guest_cid(&self) -> u64;

    /// Starts listening on a local port.
    fn listen(&mut self, src_port: u32);

    /// Initiates a connection to a peer socket.
    fn connect(&mut self, cid: VsockConnId) -> DevResult<()>;

    /// Sends data to the peer socket.
    fn send(&mut self, cid: VsockConnId, buf: &[u8]) -> DevResult<usize>;

    /// Receives data from the peer socket.
    ///
    /// Implementations may return `Err(DevError::Again)` if no data is
    /// available yet.
    fn recv(&mut self, cid: VsockConnId, buf: &mut [u8]) -> DevResult<usize>;

    /// Returns bytes currently available for `recv`.
    fn recv_avail(&mut self, cid: VsockConnId) -> DevResult<usize>;

    /// Requests a graceful shutdown of the connection.
    fn disconnect(&mut self, cid: VsockConnId) -> DevResult<()>;

    /// Forcibly closes the connection without waiting for the peer.
    fn abort(&mut self, cid: VsockConnId) -> DevResult<()>;

    /// Polls one driver event.
    ///
    /// Unknown/proprietary events should be surfaced as
    /// `VsockDriverEvent::Unknown` instead of being treated as fatal errors.
    fn poll_event(&mut self) -> DevResult<Option<VsockDriverEvent>>;
}
