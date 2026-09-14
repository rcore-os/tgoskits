//! Unix domain socket facade.
//!
//! This module provides the shared address namespace and transport dispatch for
//! Unix stream and datagram sockets. The concrete transports live in
//! `stream.rs` and `dgram.rs`; this layer handles bind/connect/accept plumbing
//! and exposes them through the common socket API.
//!
//! # Namespace Model
//!
//! Abstract names are stored in an in-memory map owned by ax-net. Path names are
//! delegated to an optional filesystem namespace provider so the socket layer
//! does not depend on a concrete VFS implementation.
//!
//! # Transport Split
//!
//! `UnixSocket` owns local/remote address state and a protocol-erased
//! `Transport`. Stream and datagram transports implement the actual byte-stream
//! or message semantics, including cmsg handling and poll readiness.

pub(crate) mod dgram;
pub mod namespace;
pub(crate) mod stream;

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

use ax_io::{IoBuf, Read, Write};
use ax_lazyinit::LazyLock;
use ax_sync::SpinLock;
use axpoll::{ExclusiveRegistrationSink, IoEvents, Pollable, SharedRegistrationSink};
use axpoll_set::PollSet;
use enum_dispatch::enum_dispatch;
use hashbrown::HashMap;

pub use self::{
    dgram::DgramTransport,
    namespace::{UnixNamespace, register_unix_namespace},
    stream::StreamTransport,
};
use crate::{
    ConnectStatus, NetError, NetResult, RecvOptions, SendOptions, Shutdown, Socket, SocketAddrEx,
    SocketOps,
    options::{Configurable, GetSocketOption, SetSocketOption},
};

/// Address for a Unix domain socket.
#[derive(Default, Clone, Debug)]
pub enum UnixSocketAddr {
    /// Unnamed (anonymous) socket.
    #[default]
    Unnamed,
    /// Abstract namespace address.
    Abstract(Arc<[u8]>),
    /// Filesystem path address.
    Path(Arc<str>),
}

/// Abstract transport trait for Unix sockets.
#[enum_dispatch]
pub trait TransportOps: Configurable + Pollable + Send + Sync {
    /// Bind the transport to the given address.
    fn bind(&self, slot: &BindSlot, local_addr: &UnixSocketAddr) -> NetResult;
    /// Connect the transport to a remote address and return an accept poll set
    /// that must be woken after the namespace and socket-state locks are released.
    fn connect(
        &self,
        slot: &BindSlot,
        local_addr: &UnixSocketAddr,
    ) -> NetResult<Option<Arc<PollSet>>>;

    /// Marks a bound connection-oriented transport as accepting connections.
    fn listen(&self) -> NetResult {
        Err(NetError::OperationNotSupported)
    }

    /// Returns whether this transport currently accepts connections.
    fn is_listening(&self) -> bool {
        false
    }

    /// Non-blocking accept: returns `WouldBlock` immediately when no connection is pending.
    fn try_accept(&self) -> NetResult<(Transport, UnixSocketAddr)> {
        Err(NetError::WouldBlock)
    }

    /// Send data through the transport.
    fn try_send(&self, src: impl Read + IoBuf, options: &mut SendOptions) -> NetResult<usize>;
    /// Receive data from the transport.
    fn try_recv(&self, dst: impl Write, options: &mut RecvOptions<'_>) -> NetResult<usize>;

    /// Shutdown the transport.
    fn shutdown(&self, _how: Shutdown) -> NetResult {
        Ok(())
    }
}

/// Unix domain transport type (stream or datagram).
#[enum_dispatch(Configurable, TransportOps)]
pub enum Transport {
    /// Stream-oriented transport.
    Stream(StreamTransport),
    /// Datagram-oriented transport.
    Dgram(DgramTransport),
}
impl Transport {
    fn finish_connect(&self, accept_poll: Option<Arc<PollSet>>) {
        if let Some(poll) = accept_poll {
            // The connection request and both endpoint states are visible, and
            // no namespace or transport lock is held while wakers run.
            unsafe { poll.wake(IoEvents::IN) };
        }
        match self {
            Transport::Stream(stream) => stream.wake_connected(),
            Transport::Dgram(dgram) => dgram.wake_connected(),
        }
    }
}
impl Pollable for Transport {
    fn poll(&self) -> IoEvents {
        match self {
            Transport::Stream(stream) => stream.poll(),
            Transport::Dgram(dgram) => dgram.poll(),
        }
    }

    unsafe fn register_shared(&self, sink: &mut dyn SharedRegistrationSink, events: IoEvents) {
        match self {
            Transport::Stream(stream) => unsafe { stream.register_shared(sink, events) },
            Transport::Dgram(dgram) => unsafe { dgram.register_shared(sink, events) },
        }
    }

    unsafe fn register_exclusive(
        &self,
        sink: &mut dyn ExclusiveRegistrationSink,
        events: IoEvents,
    ) {
        match self {
            Transport::Stream(stream) => unsafe { stream.register_exclusive(sink, events) },
            Transport::Dgram(dgram) => unsafe { dgram.register_exclusive(sink, events) },
        }
    }
}

/// Holds binding state for stream and datagram transports at a Unix address.
#[derive(Default)]
pub struct BindSlot {
    /// Stream listener bound at this address.
    stream: SpinLock<Option<stream::Bind>>,
    /// Datagram endpoint bound at this address.
    dgram: SpinLock<Option<dgram::Bind>>,
    /// Seqpacket listener bound at this address. Seqpacket is connection
    /// oriented (like stream) but preserves message boundaries (like dgram),
    /// so it carries its own connection-request queue.
    seqpacket: SpinLock<Option<dgram::SeqBind>>,
}

static ABSTRACT_BINDS: LazyLock<SpinLock<HashMap<Arc<[u8]>, BindSlot>>> =
    LazyLock::new(|| SpinLock::new(HashMap::new()));

/// Resolves an existing bind slot and runs `f` with it.
pub(crate) fn with_slot<R>(
    addr: &UnixSocketAddr,
    f: impl FnOnce(&BindSlot) -> NetResult<R>,
) -> NetResult<R> {
    match addr {
        UnixSocketAddr::Unnamed => Err(NetError::InvalidInput),
        UnixSocketAddr::Abstract(name) => {
            let binds = ABSTRACT_BINDS.lock();
            if let Some(slot) = binds.get(name) {
                f(slot)
            } else {
                Err(NetError::NotFound)
            }
        }
        UnixSocketAddr::Path(path) => namespace::with_namespace(|ns| {
            let slot = ns.resolve(path.as_ref())?;
            f(slot.as_ref())
        }),
    }
}
/// Resolves or creates a bind slot and runs `f` with it.
fn with_slot_or_insert<R>(
    addr: &UnixSocketAddr,
    f: impl FnOnce(&BindSlot) -> NetResult<R>,
) -> NetResult<R> {
    match addr {
        UnixSocketAddr::Unnamed => Err(NetError::InvalidInput),
        UnixSocketAddr::Abstract(name) => {
            let mut binds = ABSTRACT_BINDS.lock();
            f(binds.entry(name.clone()).or_default())
        }
        UnixSocketAddr::Path(path) => namespace::with_namespace(|ns| {
            let slot = ns.bind(path.as_ref())?;
            f(slot.as_ref())
        }),
    }
}

/// A Unix domain socket.
pub struct UnixSocket {
    /// Concrete stream or datagram transport.
    transport: Transport,
    /// Public local Unix address.
    local_addr: SpinLock<UnixSocketAddr>,
    /// Public remote Unix address.
    remote_addr: SpinLock<Option<UnixSocketAddr>>,
    /// Whether this socket owns the namespace binding in `local_addr`.
    ///
    /// Accepted sockets inherit the listener's local address but not ownership
    /// of its namespace entry, while duplicated descriptors share this whole
    /// socket object and therefore release the entry only on the final close.
    owns_bind: AtomicBool,
}
impl UnixSocket {
    /// Create a new Unix socket with the given transport.
    pub fn new(transport: impl Into<Transport>) -> Self {
        Self {
            transport: transport.into(),
            local_addr: SpinLock::new(UnixSocketAddr::Unnamed),
            remote_addr: SpinLock::new(None),
            owns_bind: AtomicBool::new(false),
        }
    }

    /// Create one endpoint of an already-connected anonymous socket pair.
    pub fn new_connected(transport: impl Into<Transport>) -> Self {
        Self {
            transport: transport.into(),
            local_addr: SpinLock::new(UnixSocketAddr::Unnamed),
            remote_addr: SpinLock::new(Some(UnixSocketAddr::Unnamed)),
            owns_bind: AtomicBool::new(false),
        }
    }

    fn write_connected_peer_address(&self, from: Option<&mut SocketAddrEx>) {
        if let Some(from) = from
            && let Some(peer) = self.remote_addr.lock().clone()
        {
            *from = SocketAddrEx::Unix(peer);
        }
    }
}
impl Configurable for UnixSocket {
    fn get_option_inner(&self, opt: &mut GetSocketOption) -> NetResult<bool> {
        self.transport.get_option_inner(opt)
    }

    fn set_option_inner(&self, opt: SetSocketOption) -> NetResult<bool> {
        self.transport.set_option_inner(opt)
    }
}
impl SocketOps for UnixSocket {
    fn bind(&self, local_addr: SocketAddrEx) -> NetResult {
        let local_addr = local_addr.into_unix()?;
        let mut guard = self.local_addr.lock();
        if matches!(&*guard, UnixSocketAddr::Unnamed) {
            with_slot_or_insert(&local_addr, |slot| self.transport.bind(slot, &local_addr))?;
            *guard = local_addr;
            self.owns_bind.store(true, Ordering::Release);
        } else {
            return Err(NetError::InvalidInput);
        }
        Ok(())
    }

    fn start_connect(&self, remote_addr: SocketAddrEx) -> NetResult<ConnectStatus> {
        let remote_addr = remote_addr.into_unix()?;
        let local_addr = self.local_addr.lock().clone();
        let accept_poll = {
            let mut guard = self.remote_addr.lock();
            if guard.is_some() {
                return Err(NetError::InvalidInput);
            }
            let accept_poll = with_slot(&remote_addr, |slot| {
                self.transport.connect(slot, &local_addr)
            })?;
            *guard = Some(remote_addr);
            accept_poll
        };
        self.transport.finish_connect(accept_poll);
        Ok(ConnectStatus::Connected)
    }

    fn listen(&self, _backlog: usize) -> NetResult {
        self.transport.listen()
    }

    fn is_listening(&self) -> bool {
        self.transport.is_listening()
    }

    fn try_accept(&self) -> NetResult<Socket> {
        let (transport, peer_addr) = self.transport.try_accept()?;
        Ok(Self {
            transport,
            local_addr: SpinLock::new(self.local_addr.lock().clone()),
            remote_addr: SpinLock::new(Some(peer_addr)),
            owns_bind: AtomicBool::new(false),
        }
        .into())
    }

    fn try_send(&self, src: impl Read + IoBuf, options: &mut SendOptions) -> NetResult<usize> {
        self.transport.try_send(src, options)
    }

    fn try_recv(&self, dst: impl Write, options: &mut RecvOptions<'_>) -> NetResult<usize> {
        // Linux reports the connected peer in recvfrom/recvmsg for Unix
        // stream sockets.  StreamTransport only moves bytes and ancillary
        // data, so populate the address at the facade where the connection's
        // logical peer identity is tracked.  Datagram-like transports keep
        // filling this from each packet's sender below.
        if matches!(&self.transport, Transport::Stream(_)) {
            let received = self.transport.try_recv(dst, options)?;
            self.write_connected_peer_address(options.from.as_deref_mut());
            return Ok(received);
        }
        self.transport.try_recv(dst, options)
    }

    fn local_addr(&self) -> NetResult<SocketAddrEx> {
        Ok(SocketAddrEx::Unix(self.local_addr.lock().clone()))
    }

    fn peer_addr(&self) -> NetResult<SocketAddrEx> {
        self.remote_addr
            .lock()
            .clone()
            .map(SocketAddrEx::Unix)
            .ok_or(NetError::NotConnected)
    }

    fn shutdown(&self, how: Shutdown) -> NetResult {
        self.transport.shutdown(how)
    }
}

impl Drop for UnixSocket {
    fn drop(&mut self) {
        if !self.owns_bind.load(Ordering::Acquire) {
            return;
        }
        let UnixSocketAddr::Abstract(name) = self.local_addr.get_mut() else {
            // Pathname socket nodes persist after close and are removed only by
            // an explicit filesystem unlink, matching Linux.
            return;
        };
        let removed = ABSTRACT_BINDS.lock().remove(name);
        drop(removed);
    }
}

impl Pollable for UnixSocket {
    fn poll(&self) -> IoEvents {
        self.transport.poll()
    }

    unsafe fn register_shared(&self, sink: &mut dyn SharedRegistrationSink, events: IoEvents) {
        unsafe { self.transport.register_shared(sink, events) };
    }

    unsafe fn register_exclusive(
        &self,
        sink: &mut dyn ExclusiveRegistrationSink,
        events: IoEvents,
    ) {
        unsafe { self.transport.register_exclusive(sink, events) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_address_distinguishes_unconnected_and_socketpair() {
        let unconnected = UnixSocket::new(DgramTransport::new(1));
        assert!(matches!(
            unconnected.peer_addr(),
            Err(NetError::NotConnected)
        ));

        let connected = UnixSocket::new_connected(DgramTransport::new(1));
        assert!(matches!(
            connected.peer_addr(),
            Ok(SocketAddrEx::Unix(UnixSocketAddr::Unnamed))
        ));
    }

    #[test]
    fn stream_receive_peer_address_uses_logical_remote() {
        let receiver = UnixSocket {
            transport: StreamTransport::new(1).into(),
            local_addr: SpinLock::new(UnixSocketAddr::Unnamed),
            remote_addr: SpinLock::new(Some(UnixSocketAddr::Path(Arc::from("server.sock")))),
            owns_bind: AtomicBool::new(false),
        };

        let mut from = SocketAddrEx::Unix(UnixSocketAddr::Unnamed);
        receiver.write_connected_peer_address(Some(&mut from));
        assert!(matches!(
            from,
            SocketAddrEx::Unix(UnixSocketAddr::Path(path)) if path.as_ref() == "server.sock"
        ));
    }

    #[test]
    fn abstract_bind_is_released_on_final_socket_drop() {
        let address = UnixSocketAddr::Abstract(Arc::from(&b"rebind-after-close"[..]));
        {
            let first = UnixSocket::new(DgramTransport::new(1));
            first.bind(SocketAddrEx::Unix(address.clone())).unwrap();
        }

        let second = UnixSocket::new(DgramTransport::new(2));
        second.bind(SocketAddrEx::Unix(address)).unwrap();
    }
}
