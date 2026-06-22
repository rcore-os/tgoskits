//! Vsock socket facade.
//!
//! This module exposes stream-oriented vsock through the common socket API.
//!
//! # Stack Boundary
//!
//! Vsock is not an IP protocol and is not driven through smoltcp. The facade
//! shares the same `SocketOps`, `Pollable`, and socket option plumbing as IP
//! sockets, but actual connection state lives in `connection_manager` and the
//! device event loop in `device::vsock`.

pub(crate) mod connection_manager;
pub(crate) mod stream;

use core::task::Context;

use ax_errno::{AxError, AxResult};
use ax_io::{IoBuf, IoBufMut, Read, Write};
use axpoll::{IoEvents, Pollable};
pub use rdif_vsock::{VsockAddr, VsockConnId};

pub use self::stream::VsockStreamTransport;
use crate::{
    RecvOptions, SendOptions, Shutdown, Socket, SocketAddrEx, SocketOps,
    options::{Configurable, GetSocketOption, SetSocketOption},
};

/// Operations implemented by the stream vsock transport.
pub trait VsockTransportOps: Configurable + Pollable + Send + Sync {
    /// Bind the transport to a local address.
    fn bind(&self, local_addr: VsockAddr) -> AxResult;
    /// Start listening for incoming connections.
    fn listen(&self) -> AxResult;
    /// Connect to a remote peer address.
    fn connect(&self, peer_addr: VsockAddr) -> AxResult;
    /// Accept an incoming connection.
    fn accept(&self) -> AxResult<(VsockStreamTransport, VsockAddr)>;
    /// Send data through the transport.
    fn send(&self, src: impl Read + IoBuf, options: SendOptions) -> AxResult<usize>;
    /// Receive data from the transport.
    fn recv(&self, dst: impl Write, options: RecvOptions<'_>) -> AxResult<usize>;
    /// Shutdown the transport.
    fn shutdown(&self, _how: Shutdown) -> AxResult;
    /// Get the local address, if bound.
    fn local_addr(&self) -> AxResult<Option<VsockAddr>>;
    /// Get the peer address, if connected.
    fn peer_addr(&self) -> AxResult<Option<VsockAddr>>;
}

/// A network socket using the vsock protocol.
pub struct VsockSocket {
    /// Stream-oriented vsock transport.
    transport: VsockStreamTransport,
}

impl VsockSocket {
    /// Create a new stream-oriented vsock socket.
    pub fn new() -> Self {
        Self {
            transport: VsockStreamTransport::new(),
        }
    }

    fn from_transport(transport: VsockStreamTransport) -> Self {
        Self { transport }
    }
}

impl Default for VsockSocket {
    fn default() -> Self {
        Self::new()
    }
}

impl Configurable for VsockSocket {
    fn get_option_inner(&self, opt: &mut GetSocketOption) -> AxResult<bool> {
        self.transport.get_option_inner(opt)
    }

    fn set_option_inner(&self, opt: SetSocketOption) -> AxResult<bool> {
        self.transport.set_option_inner(opt)
    }
}

impl SocketOps for VsockSocket {
    fn bind(&self, local_addr: SocketAddrEx) -> AxResult {
        let local_addr = local_addr.into_vsock()?;
        self.transport.bind(local_addr)
    }

    fn connect(&self, remote_addr: SocketAddrEx) -> AxResult {
        let remote_addr = remote_addr.into_vsock()?;
        self.transport.connect(remote_addr)
    }

    fn listen(&self, _backlog: usize) -> AxResult {
        self.transport.listen()
    }

    fn accept(&self) -> AxResult<Socket> {
        self.transport.accept().map(|(transport, _addr)| {
            let socket = VsockSocket::from_transport(transport);
            socket.into()
        })
    }

    fn send(&self, src: impl Read + IoBuf, options: SendOptions) -> AxResult<usize> {
        self.transport.send(src, options)
    }

    fn recv(&self, dst: impl Write + IoBufMut, options: RecvOptions<'_>) -> AxResult<usize> {
        self.transport.recv(dst, options)
    }

    fn local_addr(&self) -> AxResult<SocketAddrEx> {
        Ok(SocketAddrEx::Vsock(
            self.transport.local_addr()?.ok_or(AxError::NotFound)?,
        ))
    }

    fn peer_addr(&self) -> AxResult<SocketAddrEx> {
        Ok(SocketAddrEx::Vsock(
            self.transport.peer_addr()?.ok_or(AxError::NotFound)?,
        ))
    }

    fn shutdown(&self, how: Shutdown) -> AxResult {
        self.transport.shutdown(how)
    }
}

impl Pollable for VsockSocket {
    fn poll(&self) -> IoEvents {
        self.transport.poll()
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        self.transport.register(context, events);
    }
}
