//! TCP listen table and accept backlog management.
//!
//! smoltcp sockets are connection endpoints, so ax-net keeps a listener table
//! that maps bound listen endpoints to pending child sockets. Incoming TCP
//! packets create per-connection smoltcp sockets, queue them for accept(), and
//! wake the listener when a child becomes ready.
//!
//! # Why Not One smoltcp Listener Socket
//!
//! The public TCP listen socket is a stable userspace object, but smoltcp needs
//! an actual TCP socket to advance each handshake. This table bridges that
//! mismatch: the listener owns an accept queue, while each pending flow owns a
//! child smoltcp socket that can move through SYN-RECEIVED to ESTABLISHED.
//!
//! # Address Semantics
//!
//! Bind conflicts follow Linux-style wildcard behavior. A wildcard listener
//! conflicts with every specific address on the same port, while two distinct
//! specific addresses may share a port. Incoming packets are matched by port and
//! local destination address before a child is created.
//!
//! Listeners that all requested `SO_REUSEPORT` on the same local address form a
//! group and may share one endpoint, mirroring the bind-side reuseport group. A
//! listener that did not request `SO_REUSEPORT` still conflicts with the port.
//!
//! # Lock Ordering
//!
//! Callers that inspect child socket state pass a locked `SocketSet` into this
//! module. The required order is `SOCKET_SET -> listen-table bucket`; this file
//! must never acquire the outer service lock.

use alloc::{collections::VecDeque, sync::Arc, vec, vec::Vec};
use core::task::Waker;

use ax_errno::{AxError, AxResult};
use ax_sync::SpinMutex;
use axpoll::{IoEvents, PollSet};
use hashbrown::HashMap;
use smoltcp::{
    iface::{SocketHandle, SocketSet},
    socket::tcp::{self, SocketBuffer, State},
    wire::{IpEndpoint, IpListenEndpoint},
};

use crate::{
    DeferPollWake, SOCKET_SET,
    addr::listen_addrs_conflict,
    consts::{LISTEN_QUEUE_SIZE, TCP_RX_BUF_LEN, TCP_TX_BUF_LEN},
};

struct ListenTableEntryInner {
    /// Local endpoint accepted by this listener.
    listen_endpoint: IpListenEndpoint,
    /// Maximum pending child sockets.
    backlog: usize,
    /// Pending smoltcp child sockets waiting for accept().
    syn_queue: VecDeque<AcceptedTcp>,
    /// Wakes accept/poll waiters when child readiness changes.
    accept_poll: Arc<PollSet>,
    /// Whether this listener joined via `SO_REUSEPORT`, letting several
    /// listeners share the same endpoint.
    reuse_port: bool,
}

/// Child TCP socket returned by accept().
#[derive(Clone, Copy)]
pub(crate) struct AcceptedTcp {
    /// smoltcp child socket handle.
    pub(crate) handle: SocketHandle,
    /// Local endpoint observed for this connection.
    pub(crate) local_endpoint: IpEndpoint,
    /// Remote endpoint observed for this connection.
    pub(crate) remote_endpoint: IpEndpoint,
}

impl ListenTableEntryInner {
    /// Creates a listener entry and clamps backlog to the global limit.
    pub fn new(listen_endpoint: IpListenEndpoint, backlog: usize, reuse_port: bool) -> Self {
        let backlog = backlog.clamp(1, LISTEN_QUEUE_SIZE);
        Self {
            listen_endpoint,
            backlog,
            syn_queue: VecDeque::with_capacity(backlog),
            accept_poll: Arc::new(PollSet::new()),
            reuse_port,
        }
    }

    /// Returns whether an incoming packet's destination matches this listener.
    fn can_accept_endpoint(&self, dst: IpEndpoint) -> bool {
        if self.listen_endpoint.port != dst.port {
            return false;
        }
        match self.listen_endpoint.addr {
            Some(addr) => addr == dst.addr,
            None => true,
        }
    }

    /// Consumes the entry and returns all queued child handles for cleanup.
    fn into_handles(self) -> Vec<SocketHandle> {
        self.syn_queue
            .into_iter()
            .map(|pending| pending.handle)
            .collect()
    }

    /// Returns whether a child socket for this endpoint pair already exists.
    fn has_pending(&self, src: IpEndpoint, dst: IpEndpoint) -> bool {
        self.syn_queue
            .iter()
            .any(|pending| pending.local_endpoint == dst && pending.remote_endpoint == src)
    }
}

type ListenTableEntry = Arc<SpinMutex<Vec<ListenTableEntryInner>>>;

/// Per-port table of active TCP listeners.
pub struct ListenTable {
    tcp: SpinMutex<HashMap<u16, ListenTableEntry>>,
}

impl ListenTable {
    /// Creates an empty listen table indexed by TCP port.
    pub fn new() -> Self {
        Self {
            tcp: SpinMutex::new(HashMap::new()),
        }
    }

    /// Checks whether a listen endpoint can be registered.
    pub fn can_listen(&self, listen_endpoint: IpListenEndpoint) -> bool {
        let Some(entries) = self.listen_entry(listen_endpoint.port) else {
            return true;
        };
        entries
            .lock()
            .iter()
            .all(|entry| !listen_addrs_conflict(entry.listen_endpoint.addr, listen_endpoint.addr))
    }

    /// Registers a listening endpoint and backlog.
    ///
    /// Several `SO_REUSEPORT` listeners may share one endpoint: a new listener
    /// joins an existing group only when it and every colliding owner requested
    /// `SO_REUSEPORT` on the exact same local address, matching the bind-side
    /// group rule. Any other overlap returns `EADDRINUSE`.
    pub fn listen(
        &self,
        listen_endpoint: IpListenEndpoint,
        backlog: usize,
        reuse_port: bool,
    ) -> AxResult {
        let port = listen_endpoint.port;
        assert_ne!(port, 0);
        let entries = self.listen_entry_or_create(port);
        let mut entries = entries.lock();
        if entries.iter().any(|entry| {
            listen_addrs_conflict(entry.listen_endpoint.addr, listen_endpoint.addr)
                && !(reuse_port
                    && entry.reuse_port
                    && entry.listen_endpoint.addr == listen_endpoint.addr)
        }) {
            warn!("socket already listening on {}", listen_endpoint);
            return Err(AxError::AddrInUse);
        }
        entries.push(ListenTableEntryInner::new(
            listen_endpoint,
            backlog,
            reuse_port,
        ));
        Ok(())
    }

    /// Removes a listener and destroys any unaccepted child sockets.
    pub fn unlisten(&self, listen_endpoint: IpListenEndpoint) {
        debug!("TCP socket unlisten on {}", listen_endpoint);
        let (handles, remove_port) = {
            let Some(entries) = self.listen_entry(listen_endpoint.port) else {
                return;
            };
            let mut entries = entries.lock();
            let Some(idx) = entries
                .iter()
                .position(|entry| entry.listen_endpoint == listen_endpoint)
            else {
                return;
            };
            let handles = entries.swap_remove(idx).into_handles();
            (handles, entries.is_empty())
        };
        if remove_port {
            self.tcp.lock().remove(&listen_endpoint.port);
        }
        for handle in handles {
            SOCKET_SET.remove(handle);
        }
    }

    fn listen_entry(&self, port: u16) -> Option<ListenTableEntry> {
        self.tcp.lock().get(&port).cloned()
    }

    fn listen_entry_or_create(&self, port: u16) -> ListenTableEntry {
        self.tcp.lock().entry(port).or_default().clone()
    }

    // Callers pass the locked SocketSet to keep the global order:
    // SERVICE -> SOCKET_SET -> listen entry.
    /// Returns whether accept() can return a ready child socket.
    pub fn can_accept(
        &self,
        listen_endpoint: IpListenEndpoint,
        sockets: &SocketSet<'_>,
    ) -> AxResult<bool> {
        let Some(entries) = self.listen_entry(listen_endpoint.port) else {
            warn!("accept before listen");
            return Err(AxError::InvalidInput);
        };
        let table = entries.lock();
        if let Some(entry) = table
            .iter()
            .find(|entry| entry.listen_endpoint == listen_endpoint)
        {
            Ok(entry
                .syn_queue
                .iter()
                .any(|pending| is_acceptable(sockets, pending.handle)))
        } else {
            warn!("accept before listen");
            Err(AxError::InvalidInput)
        }
    }

    /// Removes and returns one acceptable child socket from the listen queue.
    pub fn accept(
        &self,
        listen_endpoint: IpListenEndpoint,
        sockets: &mut SocketSet<'_>,
    ) -> AxResult<AcceptedTcp> {
        let Some(entries) = self.listen_entry(listen_endpoint.port) else {
            warn!("accept before listen");
            return Err(AxError::InvalidInput);
        };
        let mut table = entries.lock();
        let Some(entry) = table
            .iter_mut()
            .find(|entry| entry.listen_endpoint == listen_endpoint)
        else {
            warn!("accept before listen");
            return Err(AxError::InvalidInput);
        };

        let syn_queue: &mut VecDeque<AcceptedTcp> = &mut entry.syn_queue;
        let mut idx = 0;
        while idx < syn_queue.len() {
            let handle = syn_queue[idx].handle;
            if is_closed_without_data(sockets, handle) {
                syn_queue.swap_remove_front(idx);
                sockets.remove(handle);
                continue;
            }
            if is_acceptable(sockets, handle) {
                if idx > 0 {
                    warn!(
                        "slow SYN queue enumeration: index = {}, len = {}!",
                        idx,
                        syn_queue.len()
                    );
                }
                return Ok(syn_queue.swap_remove_front(idx).unwrap());
            }
            idx += 1;
        }
        Err(AxError::WouldBlock)
    }

    /// Returns the listener readiness poll set for lock-free registration.
    pub fn accept_poll(&self, listen_endpoint: IpListenEndpoint) -> Option<Arc<PollSet>> {
        let entries = self.listen_entry(listen_endpoint.port)?;
        let table = entries.lock();
        table
            .iter()
            .find(|entry| entry.listen_endpoint == listen_endpoint)
            .map(|entry| entry.accept_poll.clone())
    }

    /// Builds the smoltcp child-progress waker for a listener poll set.
    pub fn accept_waker(&self, accept_poll: Arc<PollSet>) -> Waker {
        Waker::from(Arc::new(DeferPollWake {
            poll: accept_poll,
            ready: IoEvents::IN,
        }))
    }

    /// Registers a waker for queued child progress.
    pub fn register_pending_accept_wakers(
        &self,
        listen_endpoint: IpListenEndpoint,
        sockets: &mut SocketSet<'_>,
        accept_poll: &Arc<PollSet>,
        waker: &Waker,
    ) {
        let Some(entries) = self.listen_entry(listen_endpoint.port) else {
            return;
        };
        let table = entries.lock();
        let Some(entry) = table.iter().find(|entry| {
            entry.listen_endpoint == listen_endpoint && Arc::ptr_eq(&entry.accept_poll, accept_poll)
        }) else {
            return;
        };
        for pending in &entry.syn_queue {
            let socket: &mut tcp::Socket = sockets.get_mut(pending.handle);
            socket.register_recv_waker(waker);
            socket.register_send_waker(waker);
        }
    }

    /// Snoop hook called before smoltcp processes a potential passive open.
    pub fn incoming_tcp_packet(
        &self,
        src: IpEndpoint,
        dst: IpEndpoint,
        sockets: &mut SocketSet<'_>,
    ) {
        let Some(entries) = self.listen_entry(dst.port) else {
            return;
        };
        let wake_poll = {
            let mut table = entries.lock();
            let Some(entry) = table
                .iter_mut()
                .find(|entry| entry.can_accept_endpoint(dst))
            else {
                return;
            };
            if entry.syn_queue.len() >= entry.backlog {
                // SYN queue is full, drop the packet
                warn!("SYN queue overflow!");
                return;
            }
            if entry.has_pending(src, dst) {
                return;
            }

            // The listening socket remains a userspace-facing object. Each new
            // flow gets a child smoltcp socket so the protocol core can advance
            // the handshake independently before accept() returns it.
            let mut socket = smoltcp::socket::tcp::Socket::new(
                SocketBuffer::new(vec![0; TCP_RX_BUF_LEN]),
                SocketBuffer::new(vec![0; TCP_TX_BUF_LEN]),
            );
            if let Err(err) = socket.listen(IpListenEndpoint {
                addr: None,
                port: dst.port,
            }) {
                warn!("Failed to listen on {}: {:?}", entry.listen_endpoint, err);
                return;
            }
            let handle = sockets.add(socket);
            debug!(
                "TCP socket {}: prepare for connection {} -> {}",
                handle, src, entry.listen_endpoint
            );
            entry.syn_queue.push_back(AcceptedTcp {
                handle,
                local_endpoint: dst,
                remote_endpoint: src,
            });
            entry.accept_poll.clone()
        };
        // The child has been queued before waking accept waiters. The
        // socket-set/service locks are still held by the caller, so defer
        // the actual PollSet wake to the net worker outer loop.
        crate::defer_poll_wake(wake_poll, IoEvents::IN);
    }
}

fn is_acceptable(sockets: &SocketSet<'_>, handle: SocketHandle) -> bool {
    let socket: &tcp::Socket = sockets.get(handle);
    match socket.state() {
        State::Listen | State::SynReceived => false,
        State::Closed => socket.recv_queue() > 0,
        _ => true,
    }
}

fn is_closed_without_data(sockets: &SocketSet<'_>, handle: SocketHandle) -> bool {
    let socket: &tcp::Socket = sockets.get(handle);
    matches!(socket.state(), State::Closed) && socket.recv_queue() == 0
}

#[cfg(test)]
mod tests {
    use smoltcp::wire::{IpAddress, Ipv4Address};

    use super::*;

    fn endpoint(addr: Option<Ipv4Address>, port: u16) -> IpListenEndpoint {
        IpListenEndpoint {
            addr: addr.map(IpAddress::Ipv4),
            port,
        }
    }

    #[test]
    fn allows_same_port_on_distinct_specific_addresses() {
        let table = ListenTable::new();
        let first = endpoint(Some(Ipv4Address::new(192, 0, 2, 10)), 8080);
        let second = endpoint(Some(Ipv4Address::new(198, 51, 100, 20)), 8080);

        assert!(table.can_listen(first));
        table.listen(first, 16, false).unwrap();
        assert!(table.can_listen(second));
        table.listen(second, 16, false).unwrap();

        table.unlisten(first);
        assert!(table.can_listen(first));
        assert!(!table.can_listen(second));
    }

    #[test]
    fn wildcard_listener_conflicts_with_specific_addresses() {
        let table = ListenTable::new();
        let wildcard = endpoint(None, 8081);
        let specific = endpoint(Some(Ipv4Address::new(192, 0, 2, 10)), 8081);

        table.listen(wildcard, 16, false).unwrap();

        assert!(!table.can_listen(specific));
        assert_eq!(table.listen(specific, 16, false), Err(AxError::AddrInUse));
    }

    #[test]
    fn reuseport_group_shares_a_listen_endpoint() {
        let table = ListenTable::new();
        let ep = endpoint(Some(Ipv4Address::new(127, 0, 0, 1)), 8082);

        // Several SO_REUSEPORT listeners join the same endpoint.
        table.listen(ep, 16, true).unwrap();
        table.listen(ep, 16, true).unwrap();

        // A plain listener cannot join a reuseport group.
        assert_eq!(table.listen(ep, 16, false), Err(AxError::AddrInUse));

        // Each close removes one group member; the port frees on the last leave.
        table.unlisten(ep);
        assert_eq!(table.listen(ep, 16, false), Err(AxError::AddrInUse));
        table.unlisten(ep);
        assert!(table.can_listen(ep));
    }

    #[test]
    fn plain_listener_rejects_reuseport_join() {
        let table = ListenTable::new();
        let ep = endpoint(Some(Ipv4Address::new(127, 0, 0, 1)), 8083);

        // The first owner is plain, so even a reuseport listener still conflicts.
        table.listen(ep, 16, false).unwrap();
        assert_eq!(table.listen(ep, 16, true), Err(AxError::AddrInUse));
    }
}
