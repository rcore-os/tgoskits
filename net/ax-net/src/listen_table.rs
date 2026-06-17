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

use alloc::{boxed::Box, collections::VecDeque, sync::Arc, task::Wake, vec, vec::Vec};
use core::task::Waker;

use ax_errno::{AxError, AxResult};
use ax_sync::Mutex;
use axpoll::{IoEvents, PollSet};
use smoltcp::{
    iface::{SocketHandle, SocketSet},
    socket::tcp::{self, SocketBuffer, State},
    wire::{IpEndpoint, IpListenEndpoint},
};

use crate::{
    SOCKET_SET,
    consts::{LISTEN_QUEUE_SIZE, TCP_RX_BUF_LEN, TCP_TX_BUF_LEN},
};

const PORT_NUM: usize = 65536;

struct ListenTableEntryInner {
    /// Local endpoint accepted by this listener.
    listen_endpoint: IpListenEndpoint,
    /// Maximum pending child sockets.
    backlog: usize,
    /// Pending smoltcp child sockets waiting for accept().
    syn_queue: VecDeque<PendingTcp>,
    /// Wakes accept/poll waiters when child readiness changes.
    accept_poll: Arc<PollSet>,
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

#[derive(Clone, Copy)]
struct PendingTcp {
    accepted: AcceptedTcp,
}

struct AcceptWake {
    poll: Arc<PollSet>,
}

impl Wake for AcceptWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        // smoltcp invokes this from the net poll task context after publishing
        // child socket readiness.
        unsafe { self.poll.wake(IoEvents::IN) };
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
            .map(|pending| pending.accepted.handle)
            .collect()
    }

    /// Returns whether a child socket for this endpoint pair already exists.
    fn has_pending(&self, src: IpEndpoint, dst: IpEndpoint) -> bool {
        self.syn_queue.iter().any(|pending| {
            pending.accepted.local_endpoint == dst && pending.accepted.remote_endpoint == src
        })
    }
}

type ListenTableEntry = Arc<Mutex<Vec<ListenTableEntryInner>>>;

/// Per-port table of active TCP listeners.
pub struct ListenTable {
    tcp: Box<[ListenTableEntry]>,
}

impl ListenTable {
    /// Creates an empty listen table indexed by TCP port.
    pub fn new() -> Self {
        let tcp = unsafe {
            let mut buf = Box::new_uninit_slice(PORT_NUM);
            for i in 0..PORT_NUM {
                buf[i].write(Arc::default());
            }
            buf.assume_init()
        };
        Self { tcp }
    }

    /// Checks whether a listen endpoint can be registered.
    pub fn can_listen(&self, listen_endpoint: IpListenEndpoint) -> bool {
        self.tcp[listen_endpoint.port as usize]
            .lock()
            .iter()
            .all(|entry| !listen_addrs_conflict(entry.listen_endpoint.addr, listen_endpoint.addr))
    }

    /// Registers a listening endpoint and backlog.
    pub fn listen(&self, listen_endpoint: IpListenEndpoint, backlog: usize) -> AxResult {
        let port = listen_endpoint.port;
        assert_ne!(port, 0);
        let mut entries = self.tcp[port as usize].lock();
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
        let handles = {
            let mut entries = self.tcp[listen_endpoint.port as usize].lock();
            let Some(idx) = entries
                .iter()
                .position(|entry| entry.listen_endpoint == listen_endpoint)
            else {
                return;
            };
            entries.swap_remove(idx).into_handles()
        };
        for handle in handles {
            SOCKET_SET.remove(handle);
        }
    }

    fn listen_entry(&self, port: u16) -> Arc<Mutex<Vec<ListenTableEntryInner>>> {
        self.tcp[port as usize].clone()
    }

    // Callers pass the locked SocketSet to keep the global order:
    // SERVICE -> SOCKET_SET -> listen entry.
    /// Returns whether accept() can return a ready child socket.
    pub fn can_accept(
        &self,
        listen_endpoint: IpListenEndpoint,
        sockets: &SocketSet<'_>,
    ) -> AxResult<bool> {
        let entries = self.listen_entry(listen_endpoint.port);
        let table = entries.lock();
        if let Some(entry) = table
            .iter()
            .find(|entry| entry.listen_endpoint == listen_endpoint)
        {
            return Ok(entry
                .syn_queue
                .iter()
                .any(|pending| is_acceptable(sockets, pending.accepted.handle)));
        }
        {
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
        let entries = self.listen_entry(listen_endpoint.port);
        let mut table = entries.lock();
        let Some(entry) = table
            .iter_mut()
            .find(|entry| entry.listen_endpoint == listen_endpoint)
        else {
            warn!("accept before listen");
            return Err(AxError::InvalidInput);
        };

        let syn_queue: &mut VecDeque<PendingTcp> = &mut entry.syn_queue;
        let mut idx = 0;
        while idx < syn_queue.len() {
            let handle = syn_queue[idx].accepted.handle;
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
                return Ok(syn_queue.swap_remove_front(idx).unwrap().accepted);
            }
            idx += 1;
        }
        Err(AxError::WouldBlock)
    }

    /// Registers a waker for listener readiness and queued child progress.
    pub fn register_accept_waker(
        &self,
        listen_endpoint: IpListenEndpoint,
        sockets: &mut SocketSet<'_>,
        waker: &Waker,
    ) {
        let entries = self.listen_entry(listen_endpoint.port);
        let table = entries.lock();
        if let Some(entry) = table
            .iter()
            .find(|entry| entry.listen_endpoint == listen_endpoint)
        {
            // accept registration is performed from socket poll task context.
            unsafe { entry.accept_poll.register(waker, IoEvents::IN) };
            let accept_waker = Waker::from(Arc::new(AcceptWake {
                poll: entry.accept_poll.clone(),
            }));
            for pending in &entry.syn_queue {
                let socket: &mut tcp::Socket = sockets.get_mut(pending.accepted.handle);
                socket.register_recv_waker(&accept_waker);
                socket.register_send_waker(&accept_waker);
            }
        }
    }

    /// Snoop hook called before smoltcp processes a potential passive open.
    pub fn incoming_tcp_packet(
        &self,
        src: IpEndpoint,
        dst: IpEndpoint,
        sockets: &mut SocketSet<'_>,
    ) {
        let entries = self.listen_entry(dst.port);
        let mut table = entries.lock();
        if let Some(entry) = table
            .iter_mut()
            .find(|entry| entry.can_accept_endpoint(dst))
        {
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
            entry.syn_queue.push_back(PendingTcp {
                accepted: AcceptedTcp {
                    handle,
                    local_endpoint: dst,
                    remote_endpoint: src,
                },
            });
            // The child has been queued before waking accept waiters.
            unsafe { entry.accept_poll.wake(IoEvents::IN) };
        }
    }
}

fn listen_addrs_conflict(
    a: Option<smoltcp::wire::IpAddress>,
    b: Option<smoltcp::wire::IpAddress>,
) -> bool {
    a.is_none() || b.is_none() || a == b
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
}
