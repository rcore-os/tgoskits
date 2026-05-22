use alloc::{boxed::Box, collections::VecDeque, sync::Arc, vec, vec::Vec};
use core::ops::DerefMut;

use ax_errno::{AxError, AxResult};
use ax_sync::Mutex;
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
}

type ListenTableEntry = Arc<Mutex<Option<Box<ListenTableEntryInner>>>>;

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

    pub fn can_listen(&self, port: u16) -> bool {
        self.tcp[port as usize].lock().is_none()
    }

    pub fn listen(&self, listen_endpoint: IpListenEndpoint, backlog: usize) -> AxResult {
        let port = listen_endpoint.port;
        assert_ne!(port, 0);
        let mut entry = self.tcp[port as usize].lock();
        if entry.is_none() {
            *entry = Some(Box::new(ListenTableEntryInner::new(
                listen_endpoint,
                backlog,
            )));
            Ok(())
        } else {
            warn!("socket already listening on port {port}");
            Err(AxError::AddrInUse)
        }
    }

    pub fn unlisten(&self, port: u16) {
        debug!("TCP socket unlisten on {}", port);
        let handles = self.tcp[port as usize]
            .lock()
            .take()
            .map(|entry| (*entry).into_handles())
            .unwrap_or_default();
        for handle in handles {
            SOCKET_SET.remove(handle);
        }
    }

    fn listen_entry(&self, port: u16) -> Arc<Mutex<Option<Box<ListenTableEntryInner>>>> {
        self.tcp[port as usize].clone()
    }

    // Callers pass the locked SocketSet to keep the global order:
    // SERVICE -> SOCKET_SET -> listen entry.
    pub fn can_accept(&self, port: u16, sockets: &SocketSet<'_>) -> AxResult<bool> {
        if let Some(entry) = self.listen_entry(port).lock().as_ref() {
            Ok(entry
                .syn_queue
                .iter()
                .any(|pending| is_acceptable(sockets, pending.accepted.handle)))
        } else {
            warn!("accept before listen");
            Err(AxError::InvalidInput)
        }
    }

    pub fn accept(&self, port: u16, sockets: &mut SocketSet<'_>) -> AxResult<AcceptedTcp> {
        let entry = self.listen_entry(port);
        let mut table = entry.lock();
        let Some(entry) = table.deref_mut() else {
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

    pub fn incoming_tcp_packet(
        &self,
        src: IpEndpoint,
        dst: IpEndpoint,
        sockets: &mut SocketSet<'_>,
    ) {
        if let Some(entry) = self.listen_entry(dst.port).lock().deref_mut() {
            if !entry.can_accept_endpoint(dst) {
                return;
            }
            if entry.syn_queue.len() >= entry.backlog {
                // SYN queue is full, drop the packet
                warn!("SYN queue overflow!");
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
        }
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
