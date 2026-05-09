use alloc::{boxed::Box, vec::Vec};
use core::{
    any::Any,
    fmt::{self, Debug},
    net::SocketAddr,
    task::Context,
};

#[cfg(feature = "vsock")]
use ax_driver::prelude::VsockAddr;
use ax_errno::{AxError, AxResult, LinuxError};
use ax_io::prelude::*;
use axpoll::{IoEvents, Pollable};
use bitflags::bitflags;

#[cfg(feature = "vsock")]
use crate::vsock::VsockSocket;
use crate::{
    options::{Configurable, GetSocketOption, SetSocketOption},
    raw::RawSocket,
    tcp::TcpSocket,
    udp::UdpSocket,
    unix::{UnixSocket, UnixSocketAddr},
};

/// Extended socket address supporting IP, Unix, and vsock address families.
#[derive(Clone, Debug)]
pub enum SocketAddrEx {
    /// An IP (v4/v6) socket address.
    Ip(SocketAddr),
    /// A Unix domain socket address.
    Unix(UnixSocketAddr),
    /// A vsock socket address.
    #[cfg(feature = "vsock")]
    Vsock(VsockAddr),
}

impl SocketAddrEx {
    /// Convert into an IP socket address, or return an error if not IP.
    pub fn into_ip(self) -> AxResult<SocketAddr> {
        match self {
            SocketAddrEx::Ip(addr) => Ok(addr),
            SocketAddrEx::Unix(_) => Err(AxError::from(LinuxError::EAFNOSUPPORT)),
            #[cfg(feature = "vsock")]
            SocketAddrEx::Vsock(_) => Err(AxError::from(LinuxError::EAFNOSUPPORT)),
        }
    }

    /// Convert into a Unix socket address, or return an error if not Unix.
    pub fn into_unix(self) -> AxResult<UnixSocketAddr> {
        match self {
            SocketAddrEx::Unix(addr) => Ok(addr),
            SocketAddrEx::Ip(_) => Err(AxError::from(LinuxError::EAFNOSUPPORT)),
            #[cfg(feature = "vsock")]
            SocketAddrEx::Vsock(_) => Err(AxError::from(LinuxError::EAFNOSUPPORT)),
        }
    }

    /// Convert into a vsock address, or return an error if not vsock.
    #[cfg(feature = "vsock")]
    pub fn into_vsock(self) -> AxResult<VsockAddr> {
        match self {
            SocketAddrEx::Ip(_) => Err(AxError::from(LinuxError::EAFNOSUPPORT)),
            SocketAddrEx::Unix(_) => Err(AxError::from(LinuxError::EAFNOSUPPORT)),
            SocketAddrEx::Vsock(addr) => Ok(addr),
        }
    }
}

bitflags! {
    /// Flags for sending data to a socket.
    ///
    /// See [`SocketOps::send`].
    #[derive(Default, Debug, Clone, Copy)]
    pub struct SendFlags: u32 {
    }
}

bitflags! {
    /// Flags for receiving data from a socket.
    ///
    /// See [`SocketOps::recv`].
    #[derive(Default, Debug, Clone, Copy)]
    pub struct RecvFlags: u32 {
        /// Receive data without removing it from the queue.
        const PEEK = 0x01;
        /// For datagram-like sockets, requires [`SocketOps::recv`] to return
        /// the real size of the datagram, even when it is larger than the
        /// buffer.
        const TRUNCATE = 0x02;
    }
}

/// Type alias for ancillary control message data.
pub type CMsgData = Box<dyn Any + Send + Sync>;

/// Options for sending data to a socket.
///
/// See [`SocketOps::send`].
#[derive(Default, Debug)]
pub struct SendOptions {
    /// Destination address for the message.
    pub to: Option<SocketAddrEx>,
    /// Send flags.
    pub flags: SendFlags,
    /// Ancillary control messages.
    pub cmsg: Vec<CMsgData>,
}

/// Options for receiving data from a socket.
///
/// See [`SocketOps::recv`].
#[derive(Default)]
pub struct RecvOptions<'a> {
    /// If set, the sender's address is written here.
    pub from: Option<&'a mut SocketAddrEx>,
    /// Receive flags.
    pub flags: RecvFlags,
    /// If set, ancillary control messages are appended here.
    pub cmsg: Option<&'a mut Vec<CMsgData>>,
}
impl Debug for RecvOptions<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecvOptions")
            .field("from", &self.from)
            .field("flags", &self.flags)
            .finish()
    }
}

/// Kind of shutdown operation to perform on a socket.
#[derive(Debug, Clone, Copy)]
pub enum Shutdown {
    /// Shut down the read half.
    Read,
    /// Shut down the write half.
    Write,
    /// Shut down both halves.
    Both,
}
impl Shutdown {
    /// Returns `true` if the read half should be shut down.
    pub fn has_read(&self) -> bool {
        matches!(self, Shutdown::Read | Shutdown::Both)
    }

    /// Returns `true` if the write half should be shut down.
    pub fn has_write(&self) -> bool {
        matches!(self, Shutdown::Write | Shutdown::Both)
    }
}

/// Operations that can be performed on a socket.
pub trait SocketOps: Configurable {
    /// Binds an unbound socket to the given address and port.
    fn bind(&self, local_addr: SocketAddrEx) -> AxResult;
    /// Connects the socket to a remote address.
    fn connect(&self, remote_addr: SocketAddrEx) -> AxResult;

    /// Starts listening on the bound address and port.
    fn listen(&self, _backlog: usize) -> AxResult {
        Err(AxError::OperationNotSupported)
    }
    /// Accepts a connection on a listening socket, returning a new socket.
    fn accept(&self) -> AxResult<Socket> {
        Err(AxError::OperationNotSupported)
    }

    /// Send data to the socket, optionally to a specific address.
    fn send(&self, src: impl Read + IoBuf, options: SendOptions) -> AxResult<usize>;
    /// Receive data from the socket.
    fn recv(&self, dst: impl Write + IoBufMut, options: RecvOptions<'_>) -> AxResult<usize>;

    /// Get the local endpoint of the socket.
    fn local_addr(&self) -> AxResult<SocketAddrEx>;
    /// Get the remote endpoint of the socket.
    fn peer_addr(&self) -> AxResult<SocketAddrEx>;

    /// Shutdown the socket, closing the connection.
    fn shutdown(&self, how: Shutdown) -> AxResult;
}

impl<T: Configurable + ?Sized> Configurable for Box<T> {
    fn get_option_inner(&self, option: &mut GetSocketOption) -> AxResult<bool> {
        (**self).get_option_inner(option)
    }

    fn set_option_inner(&self, option: SetSocketOption) -> AxResult<bool> {
        (**self).set_option_inner(option)
    }
}

impl<T: SocketOps + ?Sized> SocketOps for Box<T> {
    fn bind(&self, local_addr: SocketAddrEx) -> AxResult {
        (**self).bind(local_addr)
    }

    fn connect(&self, remote_addr: SocketAddrEx) -> AxResult {
        (**self).connect(remote_addr)
    }

    fn listen(&self, backlog: usize) -> AxResult {
        (**self).listen(backlog)
    }

    fn accept(&self) -> AxResult<Socket> {
        (**self).accept()
    }

    fn send(&self, src: impl Read + IoBuf, options: SendOptions) -> AxResult<usize> {
        (**self).send(src, options)
    }

    fn recv(&self, dst: impl Write + IoBufMut, options: RecvOptions<'_>) -> AxResult<usize> {
        (**self).recv(dst, options)
    }

    fn local_addr(&self) -> AxResult<SocketAddrEx> {
        (**self).local_addr()
    }

    fn peer_addr(&self) -> AxResult<SocketAddrEx> {
        (**self).peer_addr()
    }

    fn shutdown(&self, how: Shutdown) -> AxResult {
        (**self).shutdown(how)
    }
}

/// Network socket abstraction.
pub enum Socket {
    /// UDP socket.
    Udp(Box<UdpSocket>),
    /// TCP socket.
    Tcp(Box<TcpSocket>),
    /// Raw IP socket.
    Raw(Box<RawSocket>),
    /// Unix domain socket.
    Unix(Box<UnixSocket>),
    /// Virtio socket.
    #[cfg(feature = "vsock")]
    Vsock(Box<VsockSocket>),
}

impl From<UdpSocket> for Socket {
    fn from(socket: UdpSocket) -> Self {
        Self::Udp(Box::new(socket))
    }
}

impl From<TcpSocket> for Socket {
    fn from(socket: TcpSocket) -> Self {
        Self::Tcp(Box::new(socket))
    }
}

impl From<UnixSocket> for Socket {
    fn from(socket: UnixSocket) -> Self {
        Self::Unix(Box::new(socket))
    }
}

#[cfg(feature = "vsock")]
impl From<VsockSocket> for Socket {
    fn from(socket: VsockSocket) -> Self {
        Self::Vsock(Box::new(socket))
    }
}

impl Configurable for Socket {
    fn get_option_inner(&self, opt: &mut GetSocketOption) -> AxResult<bool> {
        match self {
            Socket::Tcp(tcp) => tcp.get_option_inner(opt),
            Socket::Udp(udp) => udp.get_option_inner(opt),
            Socket::Raw(raw) => raw.get_option_inner(opt),
            Socket::Unix(unix) => unix.get_option_inner(opt),
            #[cfg(feature = "vsock")]
            Socket::Vsock(vsock) => vsock.get_option_inner(opt),
        }
    }

    fn set_option_inner(&self, opt: SetSocketOption) -> AxResult<bool> {
        match self {
            Socket::Tcp(tcp) => tcp.set_option_inner(opt),
            Socket::Udp(udp) => udp.set_option_inner(opt),
            Socket::Raw(raw) => raw.set_option_inner(opt),
            Socket::Unix(unix) => unix.set_option_inner(opt),
            #[cfg(feature = "vsock")]
            Socket::Vsock(vsock) => vsock.set_option_inner(opt),
        }
    }
}

impl SocketOps for Socket {
    fn bind(&self, local_addr: SocketAddrEx) -> AxResult {
        match self {
            Socket::Tcp(tcp) => tcp.bind(local_addr),
            Socket::Udp(udp) => udp.bind(local_addr),
            Socket::Raw(raw) => raw.bind(local_addr),
            Socket::Unix(unix) => unix.bind(local_addr),
            #[cfg(feature = "vsock")]
            Socket::Vsock(vsock) => vsock.bind(local_addr),
        }
    }

    fn connect(&self, remote_addr: SocketAddrEx) -> AxResult {
        match self {
            Socket::Tcp(tcp) => tcp.connect(remote_addr),
            Socket::Udp(udp) => udp.connect(remote_addr),
            Socket::Raw(raw) => raw.connect(remote_addr),
            Socket::Unix(unix) => unix.connect(remote_addr),
            #[cfg(feature = "vsock")]
            Socket::Vsock(vsock) => vsock.connect(remote_addr),
        }
    }

    fn listen(&self, backlog: usize) -> AxResult {
        match self {
            Socket::Tcp(tcp) => tcp.listen(backlog),
            Socket::Udp(udp) => udp.listen(backlog),
            Socket::Raw(raw) => raw.listen(backlog),
            Socket::Unix(unix) => unix.listen(backlog),
            #[cfg(feature = "vsock")]
            Socket::Vsock(vsock) => vsock.listen(backlog),
        }
    }

    fn accept(&self) -> AxResult<Socket> {
        match self {
            Socket::Tcp(tcp) => tcp.accept(),
            Socket::Udp(udp) => udp.accept(),
            Socket::Raw(raw) => raw.accept(),
            Socket::Unix(unix) => unix.accept(),
            #[cfg(feature = "vsock")]
            Socket::Vsock(vsock) => vsock.accept(),
        }
    }

    fn send(&self, src: impl Read + IoBuf, options: SendOptions) -> AxResult<usize> {
        match self {
            Socket::Tcp(tcp) => tcp.send(src, options),
            Socket::Udp(udp) => udp.send(src, options),
            Socket::Raw(raw) => raw.send(src, options),
            Socket::Unix(unix) => unix.send(src, options),
            #[cfg(feature = "vsock")]
            Socket::Vsock(vsock) => vsock.send(src, options),
        }
    }

    fn recv(&self, dst: impl Write + IoBufMut, options: RecvOptions<'_>) -> AxResult<usize> {
        match self {
            Socket::Tcp(tcp) => tcp.recv(dst, options),
            Socket::Udp(udp) => udp.recv(dst, options),
            Socket::Raw(raw) => raw.recv(dst, options),
            Socket::Unix(unix) => unix.recv(dst, options),
            #[cfg(feature = "vsock")]
            Socket::Vsock(vsock) => vsock.recv(dst, options),
        }
    }

    fn local_addr(&self) -> AxResult<SocketAddrEx> {
        match self {
            Socket::Tcp(tcp) => tcp.local_addr(),
            Socket::Udp(udp) => udp.local_addr(),
            Socket::Raw(raw) => raw.local_addr(),
            Socket::Unix(unix) => unix.local_addr(),
            #[cfg(feature = "vsock")]
            Socket::Vsock(vsock) => vsock.local_addr(),
        }
    }

    fn peer_addr(&self) -> AxResult<SocketAddrEx> {
        match self {
            Socket::Tcp(tcp) => tcp.peer_addr(),
            Socket::Udp(udp) => udp.peer_addr(),
            Socket::Raw(raw) => raw.peer_addr(),
            Socket::Unix(unix) => unix.peer_addr(),
            #[cfg(feature = "vsock")]
            Socket::Vsock(vsock) => vsock.peer_addr(),
        }
    }

    fn shutdown(&self, how: Shutdown) -> AxResult {
        match self {
            Socket::Tcp(tcp) => tcp.shutdown(how),
            Socket::Udp(udp) => udp.shutdown(how),
            Socket::Raw(raw) => raw.shutdown(how),
            Socket::Unix(unix) => unix.shutdown(how),
            #[cfg(feature = "vsock")]
            Socket::Vsock(vsock) => vsock.shutdown(how),
        }
    }
}

impl Pollable for Socket {
    fn poll(&self) -> IoEvents {
        match self {
            Socket::Tcp(tcp) => tcp.poll(),
            Socket::Udp(udp) => udp.poll(),
            Socket::Raw(raw) => raw.poll(),
            Socket::Unix(unix) => unix.poll(),
            #[cfg(feature = "vsock")]
            Socket::Vsock(vsock) => vsock.poll(),
        }
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        match self {
            Socket::Tcp(tcp) => tcp.register(context, events),
            Socket::Udp(udp) => udp.register(context, events),
            Socket::Raw(raw) => raw.register(context, events),
            Socket::Unix(unix) => unix.register(context, events),
            #[cfg(feature = "vsock")]
            Socket::Vsock(vsock) => vsock.register(context, events),
        }
    }
}
