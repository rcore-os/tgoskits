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
    namespace::{NamespaceBindSlot, UnixNamespace, register_unix_namespace},
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
    ///
    /// Returning an error must leave `slot` empty. Namespace publication is a
    /// separate transaction and is rolled back when this method rejects the
    /// bind.
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

impl BindSlot {
    fn is_empty(&self) -> bool {
        self.stream.lock().is_none() && self.dgram.lock().is_none()
    }
}

type AbstractBindMap = HashMap<Arc<[u8]>, Arc<BindSlot>>;

static ABSTRACT_BINDS: LazyLock<SpinMutex<AbstractBindMap>> =
    LazyLock::new(|| SpinMutex::new(HashMap::new()));

/// Resolves an existing bind slot without retaining a namespace lock.
pub(crate) fn resolve_slot(addr: &UnixSocketAddr) -> AxResult<Arc<BindSlot>> {
    match addr {
        UnixSocketAddr::Unnamed => Err(AxError::InvalidInput),
        UnixSocketAddr::Abstract(name) => ABSTRACT_BINDS
            .lock()
            .get(name)
            .cloned()
            .ok_or(AxError::NotFound),
        UnixSocketAddr::Path(path) => namespace::with_namespace(|ns| ns.resolve(path.as_ref())),
    }
}

/// An unpublished namespace entry reserved for one transport bind.
///
/// Dropping the reservation rolls back the abstract-map entry or filesystem
/// inode. Successful bind paths must consume it with [`Self::commit`].
struct BindReservation {
    address: UnixSocketAddr,
    slot: Arc<BindSlot>,
    remove_path_on_rollback: bool,
    active: bool,
}

impl BindReservation {
    fn reserve(address: &UnixSocketAddr) -> AxResult<Self> {
        let (slot, remove_path_on_rollback) = match address {
            UnixSocketAddr::Unnamed => return Err(AxError::InvalidInput),
            UnixSocketAddr::Abstract(name) => {
                let mut binds = ABSTRACT_BINDS.lock();
                if binds.contains_key(name) {
                    return Err(AxError::AddrInUse);
                }
                let slot = Arc::new(BindSlot::default());
                binds.insert(name.clone(), slot.clone());
                (slot, false)
            }
            UnixSocketAddr::Path(path) => {
                namespace::with_namespace(|ns| ns.reserve_bind(path.as_ref()))?.into_parts()
            }
        };
        Ok(Self {
            address: address.clone(),
            slot,
            remove_path_on_rollback,
            active: true,
        })
    }

    fn slot(&self) -> &BindSlot {
        self.slot.as_ref()
    }

    fn commit(mut self) {
        self.active = false;
    }

    fn rollback(mut self) -> AxResult {
        let result = self.rollback_inner();
        self.active = false;
        result
    }

    fn rollback_inner(&self) -> AxResult {
        if !self.slot.is_empty() {
            return Err(AxError::BadState);
        }
        match &self.address {
            UnixSocketAddr::Unnamed => Err(AxError::BadState),
            UnixSocketAddr::Abstract(name) => {
                let mut binds = ABSTRACT_BINDS.lock();
                match binds.get(name) {
                    Some(slot) if Arc::ptr_eq(slot, &self.slot) => {
                        binds.remove(name);
                        Ok(())
                    }
                    Some(_) => Err(AxError::BadState),
                    None => Ok(()),
                }
            }
            UnixSocketAddr::Path(path) if self.remove_path_on_rollback => {
                namespace::with_namespace(|ns| ns.rollback_bind(path.as_ref()))
            }
            UnixSocketAddr::Path(_) => Ok(()),
        }
    }
}

impl Drop for BindReservation {
    fn drop(&mut self) {
        if self.active
            && let Err(error) = self.rollback_inner()
        {
            error!("failed to roll back an unpublished Unix socket namespace entry: {error}");
        }
    }
}

fn publish_binding(
    state: &SpinMutex<AddressState>,
    address: UnixSocketAddr,
    publish: impl FnOnce(&BindSlot) -> AxResult,
) -> AxResult {
    let address_transaction = AddressTransaction::begin(state)?;
    let reservation = BindReservation::reserve(&address)?;
    if let Err(bind_error) = publish(reservation.slot()) {
        if let Err(rollback_error) = reservation.rollback() {
            error!(
                "Unix socket bind failed with {bind_error}; namespace rollback failed with \
                 {rollback_error}"
            );
            return Err(rollback_error);
        }
        return Err(bind_error);
    }
    reservation.commit();
    address_transaction.commit(address);
    Ok(())
}

#[derive(Default)]
enum AddressState {
    #[default]
    Unnamed,
    Busy,
    Bound(UnixSocketAddr),
}

impl AddressState {
    fn snapshot(&self) -> UnixSocketAddr {
        match self {
            Self::Bound(address) => address.clone(),
            Self::Unnamed | Self::Busy => UnixSocketAddr::Unnamed,
        }
    }

    fn from_address(address: UnixSocketAddr) -> Self {
        match address {
            UnixSocketAddr::Unnamed => Self::Unnamed,
            address => Self::Bound(address),
        }
    }
}

/// Exclusive logical ownership of one bind or connect state transition.
///
/// The spin guard is released by [`Self::begin`] before this permit is
/// returned. Slow namespace and transport work therefore runs while the state
/// is merely marked `Busy`, and dropping an uncommitted transaction restores
/// the prior `Unnamed` state.
struct AddressTransaction<'state> {
    state: &'state SpinMutex<AddressState>,
    committed: bool,
}

impl<'state> AddressTransaction<'state> {
    fn begin(state: &'state SpinMutex<AddressState>) -> AxResult<Self> {
        let mut current = state.lock();
        if !matches!(*current, AddressState::Unnamed) {
            return Err(AxError::InvalidInput);
        }
        *current = AddressState::Busy;
        drop(current);
        Ok(Self {
            state,
            committed: false,
        })
    }

    fn commit(mut self, address: UnixSocketAddr) {
        let mut current = self.state.lock();
        assert!(
            matches!(*current, AddressState::Busy),
            "Unix socket address transaction lost exclusive ownership"
        );
        *current = AddressState::Bound(address);
        self.committed = true;
    }
}

impl Drop for AddressTransaction<'_> {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let mut current = self.state.lock();
        assert!(
            matches!(*current, AddressState::Busy),
            "Unix socket address rollback lost exclusive ownership"
        );
        *current = AddressState::Unnamed;
    }
}

fn publish_address(
    state: &SpinMutex<AddressState>,
    address: UnixSocketAddr,
    publish: impl FnOnce() -> AxResult,
) -> AxResult {
    let transaction = AddressTransaction::begin(state)?;
    publish()?;
    transaction.commit(address);
    Ok(())
}

/// A Unix domain socket.
pub struct UnixSocket {
    /// Concrete stream or datagram transport.
    transport: Transport,
    /// Public local Unix address.
    local_addr: SpinMutex<AddressState>,
    /// Public remote Unix address.
    remote_addr: SpinMutex<AddressState>,
}
impl UnixSocket {
    /// Create a new Unix socket with the given transport.
    pub fn new(transport: impl Into<Transport>) -> Self {
        Self {
            transport: transport.into(),
            local_addr: SpinMutex::new(AddressState::Unnamed),
            remote_addr: SpinMutex::new(AddressState::Unnamed),
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
        publish_binding(&self.local_addr, local_addr.clone(), |slot| {
            self.transport.bind(slot, &local_addr)
        })
    }

    fn connect(&self, remote_addr: SocketAddrEx) -> AxResult {
        let remote_addr = remote_addr.into_unix()?;
        let local_addr = self.local_addr.lock().snapshot();
        publish_address(&self.remote_addr, remote_addr.clone(), || {
            let slot = resolve_slot(&remote_addr)?;
            self.transport.connect(slot.as_ref(), &local_addr)
        })
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
            local_addr: SpinMutex::new(AddressState::from_address(
                self.local_addr.lock().snapshot(),
            )),
            remote_addr: SpinMutex::new(AddressState::from_address(peer_addr)),
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
        Ok(SocketAddrEx::Unix(self.local_addr.lock().snapshot()))
    }

    fn peer_addr(&self) -> AxResult<SocketAddrEx> {
        Ok(SocketAddrEx::Unix(self.remote_addr.lock().snapshot()))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn path_address(path: &'static str) -> UnixSocketAddr {
        UnixSocketAddr::Path(path.into())
    }

    #[test]
    fn address_transaction_commits_success() {
        let state = SpinMutex::new(AddressState::Unnamed);

        publish_address(&state, path_address("/success"), || Ok(())).unwrap();

        assert!(
            matches!(&*state.lock(), AddressState::Bound(UnixSocketAddr::Path(path)) if path.as_ref() == "/success")
        );
    }

    #[test]
    fn failed_address_transaction_rolls_back_to_unnamed() {
        let state = SpinMutex::new(AddressState::Unnamed);

        assert_eq!(
            publish_address(&state, path_address("/failure"), || Err(AxError::Io)),
            Err(AxError::Io)
        );

        assert!(matches!(*state.lock(), AddressState::Unnamed));
    }

    #[test]
    fn concurrent_address_transaction_has_one_winner() {
        let state = SpinMutex::new(AddressState::Unnamed);
        let transaction = AddressTransaction::begin(&state).unwrap();

        assert!(AddressTransaction::begin(&state).is_err());
        transaction.commit(path_address("/winner"));

        assert!(
            matches!(&*state.lock(), AddressState::Bound(UnixSocketAddr::Path(path)) if path.as_ref() == "/winner")
        );
    }

    #[test]
    fn address_callback_can_reenter_the_state_lock() {
        let state = SpinMutex::new(AddressState::Unnamed);

        publish_address(&state, path_address("/reentrant"), || {
            assert!(matches!(*state.lock(), AddressState::Busy));
            assert!(matches!(state.lock().snapshot(), UnixSocketAddr::Unnamed));
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn failed_abstract_bind_does_not_publish_a_namespace_slot() {
        let address = UnixSocketAddr::Abstract(Arc::from(&b"ax-net-bind-transaction-failure"[..]));
        let state = SpinMutex::new(AddressState::Unnamed);

        assert_eq!(
            publish_binding(&state, address.clone(), |_| Err(AxError::Io)),
            Err(AxError::Io)
        );

        assert!(matches!(resolve_slot(&address), Err(AxError::NotFound)));
        assert!(matches!(*state.lock(), AddressState::Unnamed));

        publish_binding(&state, address.clone(), |_| Ok(())).unwrap();
        assert!(resolve_slot(&address).is_ok());
    }
}
