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

use alloc::{boxed::Box, sync::Arc};
use core::task::Context;

use async_trait::async_trait;
use ax_errno::{AxError, AxResult};
use ax_io::{IoBuf, Read, Write};
use ax_kspin::PreemptLazy as LazyLock;
use ax_sync::SpinMutex;
use axpoll::{IoEvents, Pollable};
use enum_dispatch::enum_dispatch;
use hashbrown::HashMap;

pub use self::{
    dgram::DgramTransport,
    namespace::{UnixNamespace, register_unix_namespace},
    stream::StreamTransport,
};
use crate::{
    RecvOptions, SendOptions, Shutdown, Socket, SocketAddrEx, SocketOps,
    blocking::poll_io,
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
#[async_trait]
#[enum_dispatch]
pub trait TransportOps: Configurable + Pollable + Send + Sync {
    /// Bind the transport to the given address.
    fn bind(&self, slot: &BindSlot, local_addr: &UnixSocketAddr) -> AxResult;
    /// Connect the transport to a remote address.
    fn connect(&self, slot: &BindSlot, local_addr: &UnixSocketAddr) -> AxResult;

    /// Accept an incoming connection, returning the new transport and peer address.
    async fn accept(&self) -> AxResult<(Transport, UnixSocketAddr)>;

    /// Non-blocking accept: returns `WouldBlock` immediately when no connection is pending.
    fn try_accept(&self) -> AxResult<(Transport, UnixSocketAddr)> {
        Err(AxError::WouldBlock)
    }

    /// Send data through the transport.
    fn send(&self, src: impl Read + IoBuf, options: SendOptions) -> AxResult<usize>;
    /// Receive data from the transport.
    fn recv(&self, dst: impl Write, options: RecvOptions<'_>) -> AxResult<usize>;

    /// Shutdown the transport.
    fn shutdown(&self, _how: Shutdown) -> AxResult {
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
impl Pollable for Transport {
    fn poll(&self) -> IoEvents {
        match self {
            Transport::Stream(stream) => stream.poll(),
            Transport::Dgram(dgram) => dgram.poll(),
        }
    }

    fn register(&self, context: &mut core::task::Context<'_>, events: IoEvents) {
        match self {
            Transport::Stream(stream) => stream.register(context, events),
            Transport::Dgram(dgram) => dgram.register(context, events),
        }
    }
}

/// Holds binding state for stream and datagram transports at a Unix address.
#[derive(Default)]
pub struct BindSlot {
    /// Stream listener bound at this address.
    stream: SpinMutex<Option<stream::Bind>>,
    /// Datagram endpoint bound at this address.
    dgram: SpinMutex<Option<dgram::Bind>>,
}

static ABSTRACT_BINDS: LazyLock<SpinMutex<HashMap<Arc<[u8]>, BindSlot>>> =
    LazyLock::new(|| SpinMutex::new(HashMap::new()));

/// Resolves an existing bind slot and runs `f` with it.
pub(crate) fn with_slot<R>(
    addr: &UnixSocketAddr,
    f: impl FnOnce(&BindSlot) -> AxResult<R>,
) -> AxResult<R> {
    match addr {
        UnixSocketAddr::Unnamed => Err(AxError::InvalidInput),
        UnixSocketAddr::Abstract(name) => {
            let binds = ABSTRACT_BINDS.lock();
            if let Some(slot) = binds.get(name) {
                f(slot)
            } else {
                Err(AxError::NotFound)
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
    f: impl FnOnce(&BindSlot) -> AxResult<R>,
) -> AxResult<R> {
    match addr {
        UnixSocketAddr::Unnamed => Err(AxError::InvalidInput),
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
    local_addr: SpinMutex<UnixSocketAddr>,
    /// Public remote Unix address.
    remote_addr: SpinMutex<UnixSocketAddr>,
}
impl UnixSocket {
    /// Create a new Unix socket with the given transport.
    pub fn new(transport: impl Into<Transport>) -> Self {
        Self {
            transport: transport.into(),
            local_addr: SpinMutex::new(UnixSocketAddr::Unnamed),
            remote_addr: SpinMutex::new(UnixSocketAddr::Unnamed),
        }
    }
}
impl Configurable for UnixSocket {
    fn get_option_inner(&self, opt: &mut GetSocketOption) -> AxResult<bool> {
        self.transport.get_option_inner(opt)
    }

    fn set_option_inner(&self, opt: SetSocketOption) -> AxResult<bool> {
        self.transport.set_option_inner(opt)
    }
}
impl SocketOps for UnixSocket {
    fn bind(&self, local_addr: SocketAddrEx) -> AxResult {
        let local_addr = local_addr.into_unix()?;
        let mut guard = self.local_addr.lock();
        if matches!(&*guard, UnixSocketAddr::Unnamed) {
            with_slot_or_insert(&local_addr, |slot| self.transport.bind(slot, &local_addr))?;
            *guard = local_addr;
        } else {
            return Err(AxError::InvalidInput);
        }
        Ok(())
    }

    fn connect(&self, remote_addr: SocketAddrEx) -> AxResult {
        let remote_addr = remote_addr.into_unix()?;
        let local_addr = self.local_addr.lock().clone();
        let mut guard = self.remote_addr.lock();
        if matches!(&*guard, UnixSocketAddr::Unnamed) {
            with_slot(&remote_addr, |slot| {
                self.transport.connect(slot, &local_addr)
            })?;
            *guard = remote_addr;
        } else {
            return Err(AxError::InvalidInput);
        }
        Ok(())
    }

    fn listen(&self, _backlog: usize) -> AxResult {
        Ok(())
    }

    fn accept(&self) -> AxResult<Socket> {
        let mut nonblocking = false;
        let _ = self
            .transport
            .get_option_inner(&mut GetSocketOption::NonBlocking(&mut nonblocking));
        let (transport, peer_addr) =
            poll_io(&self.transport, IoEvents::IN, nonblocking, None, || {
                self.transport.try_accept()
            })?;
        Ok(Self {
            transport,
            local_addr: SpinMutex::new(self.local_addr.lock().clone()),
            remote_addr: SpinMutex::new(peer_addr),
        }
        .into())
    }

    fn send(&self, src: impl Read + IoBuf, options: SendOptions) -> AxResult<usize> {
        self.transport.send(src, options)
    }

    fn recv(&self, dst: impl Write, options: RecvOptions<'_>) -> AxResult<usize> {
        self.transport.recv(dst, options)
    }

    fn local_addr(&self) -> AxResult<SocketAddrEx> {
        Ok(SocketAddrEx::Unix(self.local_addr.lock().clone()))
    }

    fn peer_addr(&self) -> AxResult<SocketAddrEx> {
        Ok(SocketAddrEx::Unix(self.remote_addr.lock().clone()))
    }

    fn shutdown(&self, how: Shutdown) -> AxResult {
        self.transport.shutdown(how)
    }
}

impl Pollable for UnixSocket {
    fn poll(&self) -> IoEvents {
        self.transport.poll()
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        self.transport.register(context, events);
    }
}
