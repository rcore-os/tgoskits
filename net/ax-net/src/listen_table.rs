use alloc::{boxed::Box, collections::VecDeque, sync::Arc, vec, vec::Vec};
use core::task::Waker;

use ax_errno::{AxError, AxResult};
use ax_sync::Mutex;
use axpoll::PollSet;
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
    listen_endpoint: IpListenEndpoint,
    backlog: usize,
    syn_queue: VecDeque<PendingTcp>,
    accept_poll: Arc<PollSet>,
}

#[derive(Clone, Copy)]
pub(crate) struct AcceptedTcp {
    pub(crate) handle: SocketHandle,
    pub(crate) local_endpoint: IpEndpoint,
    pub(crate) remote_endpoint: IpEndpoint,
}

#[derive(Clone, Copy)]
struct PendingTcp {
    accepted: AcceptedTcp,
}

impl ListenTableEntryInner {
    pub fn new(listen_endpoint: IpListenEndpoint, backlog: usize) -> Self {
        let backlog = backlog.clamp(1, LISTEN_QUEUE_SIZE);
        Self {
            listen_endpoint,
            backlog,
            syn_queue: VecDeque::with_capacity(backlog),
            accept_poll: Arc::new(PollSet::new()),
        }
    }

    fn can_accept_endpoint(&self, dst: IpEndpoint) -> bool {
        if self.listen_endpoint.port != dst.port {
            return false;
        }
        match self.listen_endpoint.addr {
            Some(addr) => addr == dst.addr,
            None => true,
        }
    }

    fn into_handles(self) -> Vec<SocketHandle> {
        self.syn_queue
            .into_iter()
            .map(|pending| pending.accepted.handle)
            .collect()
    }

    fn has_pending(&self, src: IpEndpoint, dst: IpEndpoint) -> bool {
        self.syn_queue.iter().any(|pending| {
            pending.accepted.local_endpoint == dst && pending.accepted.remote_endpoint == src
        })
    }
}

type ListenTableEntry = Arc<Mutex<Vec<ListenTableEntryInner>>>;

pub struct ListenTable {
    tcp: Box<[ListenTableEntry]>,
}

impl ListenTable {
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

    pub fn can_listen(&self, listen_endpoint: IpListenEndpoint) -> bool {
        self.tcp[listen_endpoint.port as usize]
            .lock()
            .iter()
            .all(|entry| !listen_addrs_conflict(entry.listen_endpoint.addr, listen_endpoint.addr))
    }

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
            entry.accept_poll.register(waker);
            let accept_waker = Waker::from(entry.accept_poll.clone());
            for pending in &entry.syn_queue {
                let socket: &mut tcp::Socket = sockets.get_mut(pending.accepted.handle);
                socket.register_recv_waker(&accept_waker);
                socket.register_send_waker(&accept_waker);
            }
        }
    }

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
            if !entry.can_accept_endpoint(dst) {
                return;
            }
            if entry.syn_queue.len() >= entry.backlog {
                // SYN queue is full, drop the packet
                warn!("SYN queue overflow!");
                return;
            }
            if entry.has_pending(src, dst) {
                return;
            }

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
            entry.accept_poll.wake();
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
