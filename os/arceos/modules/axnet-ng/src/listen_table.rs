use alloc::{boxed::Box, collections::VecDeque, sync::Arc, vec};
use core::ops::DerefMut;

use ax_errno::{AxError, AxResult};
use ax_sync::Mutex;
use smoltcp::{
    iface::{SocketHandle, SocketSet},
    socket::tcp::{self, SocketBuffer, State},
    wire::{IpAddress, IpEndpoint, IpListenEndpoint},
};

use crate::{
    SOCKET_SET,
    consts::{LISTEN_QUEUE_SIZE, TCP_RX_BUF_LEN, TCP_TX_BUF_LEN},
};

const PORT_NUM: usize = 65536;

struct ListenTableEntryInner {
    listen_endpoint: IpListenEndpoint,
    syn_queue: VecDeque<SocketHandle>,
    is_ipv6: bool,
    v6only: bool,
}

impl ListenTableEntryInner {
    pub fn new(listen_endpoint: IpListenEndpoint, is_ipv6: bool, v6only: bool) -> Self {
        Self {
            listen_endpoint,
            syn_queue: VecDeque::with_capacity(LISTEN_QUEUE_SIZE),
            is_ipv6,
            v6only,
        }
    }
}

impl Drop for ListenTableEntryInner {
    fn drop(&mut self) {
        for &handle in &self.syn_queue {
            SOCKET_SET.remove(handle);
        }
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

    pub fn listen(
        &self,
        listen_endpoint: IpListenEndpoint,
        is_ipv6: bool,
        v6only: bool,
    ) -> AxResult {
        let port = listen_endpoint.port;
        assert_ne!(port, 0);
        let mut entry = self.tcp[port as usize].lock();
        if entry.is_none() {
            *entry = Some(Box::new(ListenTableEntryInner::new(
                listen_endpoint,
                is_ipv6,
                v6only,
            )));
            Ok(())
        } else {
            warn!("socket already listening on port {port}");
            Err(AxError::AddrInUse)
        }
    }

    pub fn unlisten(&self, port: u16) {
        debug!("TCP socket unlisten on {}", port);
        *self.tcp[port as usize].lock() = None;
    }

    fn listen_entry(&self, port: u16) -> Arc<Mutex<Option<Box<ListenTableEntryInner>>>> {
        self.tcp[port as usize].clone()
    }

    pub fn can_accept(&self, port: u16) -> AxResult<bool> {
        if let Some(entry) = self.listen_entry(port).lock().as_ref() {
            Ok(entry.syn_queue.iter().any(|&handle| is_connected(handle)))
        } else {
            warn!("accept before listen");
            Err(AxError::InvalidInput)
        }
    }

    pub fn accept(&self, port: u16) -> AxResult<(SocketHandle, bool)> {
        let entry = self.listen_entry(port);
        let mut table = entry.lock();
        let Some(entry) = table.deref_mut() else {
            warn!("accept before listen");
            return Err(AxError::InvalidInput);
        };

        let is_ipv6 = entry.is_ipv6;
        let syn_queue: &mut VecDeque<SocketHandle> = &mut entry.syn_queue;
        let idx = syn_queue
            .iter()
            .enumerate()
            .find_map(|(idx, &handle)| is_connected(handle).then_some(idx))
            .ok_or(AxError::WouldBlock)?; // wait for connection
        if idx > 0 {
            warn!(
                "slow SYN queue enumeration: index = {}, len = {}!",
                idx,
                syn_queue.len()
            );
        }
        let handle = syn_queue.swap_remove_front(idx).unwrap();
        // If the connection is reset, return ConnectionReset error
        if is_closed(handle) {
            warn!("accept failed: connection reset");
            Err(AxError::ConnectionReset)
        } else {
            Ok((handle, is_ipv6))
        }
    }

    pub fn incoming_tcp_packet(
        &self,
        src: IpEndpoint,
        dst: IpEndpoint,
        sockets: &mut SocketSet<'_>,
    ) {
        if let Some(entry) = self.listen_entry(dst.port).lock().deref_mut() {
            // IPV6_V6ONLY: reject IPv4 connections on an IPv6-only socket
            if entry.v6only && matches!(dst.addr, IpAddress::Ipv4(_)) {
                return;
            }
            if entry.syn_queue.len() >= LISTEN_QUEUE_SIZE {
                // SYN queue is full, drop the packet
                warn!("SYN queue overflow!");
                return;
            }

            let mut socket = smoltcp::socket::tcp::Socket::new(
                SocketBuffer::new(vec![0; TCP_RX_BUF_LEN]),
                SocketBuffer::new(vec![0; TCP_TX_BUF_LEN]),
            );
            // Stamp the bound endpoint with the concrete destination address from
            // the SYN packet so that getsockname() on accepted sockets returns the
            // correct family/address (e.g. AF_INET6 for ::1 connections).
            socket.set_bound_endpoint(IpListenEndpoint {
                addr: Some(dst.addr),
                port: dst.port,
            });
            if let Err(err) = socket.listen(IpListenEndpoint {
                addr: entry.listen_endpoint.addr,
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
            entry.syn_queue.push_back(handle);
        }
    }
}

fn is_connected(handle: SocketHandle) -> bool {
    SOCKET_SET.with_socket::<tcp::Socket, _, _>(handle, |socket| {
        !matches!(socket.state(), State::Listen | State::SynReceived)
    })
}

fn is_closed(handle: SocketHandle) -> bool {
    SOCKET_SET
        .with_socket::<tcp::Socket, _, _>(handle, |socket| matches!(socket.state(), State::Closed))
}
