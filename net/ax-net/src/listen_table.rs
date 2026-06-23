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
//! # Lock Ordering
//!
//! Callers that inspect child socket state pass a locked `SocketSet` into this
//! module. The required order is `SOCKET_SET -> listen-table bucket`; this file
//! must never acquire the outer service lock.

use alloc::{collections::VecDeque, sync::Arc, vec, vec::Vec};
use core::{
    hash::{Hash, Hasher},
    sync::atomic::{AtomicUsize, Ordering},
    task::Waker,
};

use ax_errno::{AxError, AxResult};
use ax_sync::Mutex;
use axpoll::{IoEvents, PollSet};
use hashbrown::{HashMap, HashSet};
use smoltcp::{
    iface::{SocketHandle, SocketSet},
    socket::tcp::{self, SocketBuffer, State},
    wire::{IpEndpoint, IpListenEndpoint},
};

use crate::{
    DeferPollWake, SOCKET_SET,
    addr::listen_addrs_conflict,
    consts::{LISTEN_QUEUE_SIZE, TCP_LISTEN_RX_BUF_LEN, TCP_LISTEN_TX_BUF_LEN},
};

static GLOBAL_PENDING_CHILDREN: AtomicUsize = AtomicUsize::new(0);

struct ListenTableEntryInner {
    /// Local endpoint accepted by this listener.
    listen_endpoint: IpListenEndpoint,
    /// Maximum pending child sockets.
    backlog: usize,
    /// Pending smoltcp child sockets still completing TCP handshake.
    syn_queue: VecDeque<AcceptedTcp>,
    /// Child sockets ready for accept().
    ready_queue: VecDeque<AcceptedTcp>,
    /// Endpoint pairs queued in either syn_queue or ready_queue.
    pending_keys: HashSet<PendingKey>,
    /// Wakes accept/poll waiters when child readiness changes.
    accept_poll: Arc<PollSet>,
}

#[derive(Clone, Copy, Eq)]
struct PendingKey {
    src: IpEndpoint,
    dst: IpEndpoint,
}

impl PendingKey {
    fn new(src: IpEndpoint, dst: IpEndpoint) -> Self {
        Self { src, dst }
    }
}

impl PartialEq for PendingKey {
    fn eq(&self, other: &Self) -> bool {
        self.src == other.src && self.dst == other.dst
    }
}

impl Hash for PendingKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.src.hash(state);
        self.dst.hash(state);
    }
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

impl AcceptedTcp {
    fn pending_key(self) -> PendingKey {
        PendingKey::new(self.remote_endpoint, self.local_endpoint)
    }
}

impl ListenTableEntryInner {
    /// Creates a listener entry and clamps backlog to the global limit.
    pub fn new(listen_endpoint: IpListenEndpoint, backlog: usize) -> Self {
        let backlog = backlog.clamp(1, LISTEN_QUEUE_SIZE);
        Self {
            listen_endpoint,
            backlog,
            syn_queue: VecDeque::with_capacity(backlog),
            ready_queue: VecDeque::with_capacity(backlog),
            pending_keys: HashSet::with_capacity(backlog),
            accept_poll: Arc::new(PollSet::new()),
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
            .chain(self.ready_queue)
            .map(|pending| pending.handle)
            .collect()
    }

    /// Returns whether a child socket for this endpoint pair already exists.
    fn has_pending(&self, src: IpEndpoint, dst: IpEndpoint) -> bool {
        self.pending_keys.contains(&PendingKey::new(src, dst))
    }

    fn queued_len(&self) -> usize {
        self.syn_queue.len() + self.ready_queue.len()
    }

    fn push_syn(&mut self, accepted: AcceptedTcp) {
        self.pending_keys.insert(accepted.pending_key());
        self.syn_queue.push_back(accepted);
        GLOBAL_PENDING_CHILDREN.fetch_add(1, Ordering::Relaxed);
    }

    fn pop_ready(&mut self) -> Option<AcceptedTcp> {
        let accepted = self.ready_queue.pop_front()?;
        self.pending_keys.remove(&accepted.pending_key());
        GLOBAL_PENDING_CHILDREN.fetch_sub(1, Ordering::Relaxed);
        Some(accepted)
    }

    fn remove_syn_at(&mut self, idx: usize) -> Option<AcceptedTcp> {
        let accepted = self.syn_queue.remove(idx)?;
        self.pending_keys.remove(&accepted.pending_key());
        GLOBAL_PENDING_CHILDREN.fetch_sub(1, Ordering::Relaxed);
        Some(accepted)
    }

    fn mark_ready_at(&mut self, idx: usize) -> Option<AcceptedTcp> {
        let accepted = self.syn_queue.remove(idx)?;
        self.ready_queue.push_back(accepted);
        Some(accepted)
    }
}

type ListenTableEntry = Arc<Mutex<Vec<ListenTableEntryInner>>>;

/// Per-port table of active TCP listeners.
pub struct ListenTable {
    tcp: Mutex<HashMap<u16, ListenTableEntry>>,
}

impl ListenTable {
    /// Creates an empty listen table indexed by TCP port.
    pub fn new() -> Self {
        Self {
            tcp: Mutex::new(HashMap::new()),
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
    pub fn listen(&self, listen_endpoint: IpListenEndpoint, backlog: usize) -> AxResult {
        let port = listen_endpoint.port;
        assert_ne!(port, 0);
        let entries = self.listen_entry_or_create(port);
        let mut entries = entries.lock();
        if entries
            .iter()
            .any(|entry| listen_addrs_conflict(entry.listen_endpoint.addr, listen_endpoint.addr))
        {
            warn!("socket already listening on {}", listen_endpoint);
            return Err(AxError::AddrInUse);
        }
        entries.push(ListenTableEntryInner::new(listen_endpoint, backlog));
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
            GLOBAL_PENDING_CHILDREN.fetch_sub(handles.len(), Ordering::Relaxed);
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
                .ready_queue
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

        while let Some(accepted) = entry.pop_ready() {
            if is_closed_without_data(sockets, accepted.handle) {
                sockets.remove(accepted.handle);
                continue;
            }
            return Ok(accepted);
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
        for pending in entry.syn_queue.iter().chain(&entry.ready_queue) {
            let socket: &mut tcp::Socket = sockets.get_mut(pending.handle);
            socket.register_recv_waker(waker);
            socket.register_send_waker(waker);
        }
    }

    /// Advances pending child sockets into the ready queue or removes closed ones.
    pub fn process_pending(&self, sockets: &mut SocketSet<'_>) -> bool {
        let entries = self.tcp.lock().values().cloned().collect::<Vec<_>>();
        let mut wake_polls = Vec::new();
        let mut removed = Vec::new();
        for entries in entries {
            let mut table = entries.lock();
            for entry in table.iter_mut() {
                let mut idx = 0;
                while idx < entry.syn_queue.len() {
                    let handle = entry.syn_queue[idx].handle;
                    if is_closed_without_data(sockets, handle) {
                        if let Some(accepted) = entry.remove_syn_at(idx) {
                            removed.push(accepted.handle);
                        }
                        continue;
                    }
                    if is_acceptable(sockets, handle) {
                        entry.mark_ready_at(idx);
                        wake_polls.push(entry.accept_poll.clone());
                        continue;
                    }
                    idx += 1;
                }
            }
        }
        for handle in removed {
            sockets.remove(handle);
        }
        let has_wake = !wake_polls.is_empty();
        for poll in wake_polls {
            crate::defer_poll_wake(poll, IoEvents::IN);
        }
        has_wake
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
            if entry.queued_len() >= entry.backlog {
                // SYN queue is full, drop the packet
                warn!("SYN queue overflow!");
                return;
            }
            if GLOBAL_PENDING_CHILDREN.load(Ordering::Relaxed) >= LISTEN_QUEUE_SIZE {
                warn!("global SYN queue overflow!");
                return;
            }
            if entry.has_pending(src, dst) {
                return;
            }

            // The listening socket remains a userspace-facing object. Each new
            // flow gets a child smoltcp socket so the protocol core can advance
            // the handshake independently before accept() returns it.
            let mut socket = smoltcp::socket::tcp::Socket::new(
                SocketBuffer::new(vec![0; TCP_LISTEN_RX_BUF_LEN]),
                SocketBuffer::new(vec![0; TCP_LISTEN_TX_BUF_LEN]),
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
            entry.push_syn(AcceptedTcp {
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
        table.listen(first, 16).unwrap();
        assert!(table.can_listen(second));
        table.listen(second, 16).unwrap();

        table.unlisten(first);
        assert!(table.can_listen(first));
        assert!(!table.can_listen(second));
    }

    #[test]
    fn wildcard_listener_conflicts_with_specific_addresses() {
        let table = ListenTable::new();
        let wildcard = endpoint(None, 8081);
        let specific = endpoint(Some(Ipv4Address::new(192, 0, 2, 10)), 8081);

        table.listen(wildcard, 16).unwrap();

        assert!(!table.can_listen(specific));
        assert_eq!(table.listen(specific, 16), Err(AxError::AddrInUse));
    }

    #[test]
    fn pending_keys_cover_syn_and_ready_queues() {
        let mut entry = ListenTableEntryInner::new(endpoint(None, 8082), 16);
        let src = IpEndpoint::new(IpAddress::Ipv4(Ipv4Address::new(198, 51, 100, 1)), 50000);
        let dst = IpEndpoint::new(IpAddress::Ipv4(Ipv4Address::new(192, 0, 2, 1)), 8082);
        let accepted = AcceptedTcp {
            handle: SocketHandle::default(),
            local_endpoint: dst,
            remote_endpoint: src,
        };

        entry.push_syn(accepted);
        assert!(entry.has_pending(src, dst));
        assert_eq!(entry.queued_len(), 1);

        entry.mark_ready_at(0).unwrap();
        assert!(entry.has_pending(src, dst));
        assert_eq!(entry.queued_len(), 1);

        assert_eq!(entry.pop_ready().unwrap().remote_endpoint, src);
        assert!(!entry.has_pending(src, dst));
        assert_eq!(entry.queued_len(), 0);
    }

    #[test]
    fn ready_queue_preserves_fifo_order() {
        let mut entry = ListenTableEntryInner::new(endpoint(None, 8083), 16);
        let dst = IpEndpoint::new(IpAddress::Ipv4(Ipv4Address::new(192, 0, 2, 1)), 8083);
        for i in 1..=3 {
            entry.push_syn(AcceptedTcp {
                handle: SocketHandle::default(),
                local_endpoint: dst,
                remote_endpoint: IpEndpoint::new(
                    IpAddress::Ipv4(Ipv4Address::new(198, 51, 100, i as u8)),
                    50000 + i as u16,
                ),
            });
        }

        entry.mark_ready_at(0).unwrap();
        entry.mark_ready_at(0).unwrap();
        entry.mark_ready_at(0).unwrap();

        assert_eq!(
            entry.pop_ready().unwrap().remote_endpoint.addr,
            IpAddress::Ipv4(Ipv4Address::new(198, 51, 100, 1))
        );
        assert_eq!(
            entry.pop_ready().unwrap().remote_endpoint.addr,
            IpAddress::Ipv4(Ipv4Address::new(198, 51, 100, 2))
        );
        assert_eq!(
            entry.pop_ready().unwrap().remote_endpoint.addr,
            IpAddress::Ipv4(Ipv4Address::new(198, 51, 100, 3))
        );
    }
}
