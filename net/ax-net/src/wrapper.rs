use alloc::vec;

use ax_errno::{AxError, AxResult};
use ax_sync::Mutex;
use event_listener::Event;
use hashbrown::HashMap;
use smoltcp::{
    iface::{SocketHandle, SocketSet},
    socket::AnySocket,
    wire::IpAddress,
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct UdpBindKey {
    addr: Option<IpAddress>,
    port: u16,
}

pub(crate) struct SocketSetWrapper<'a> {
    pub inner: Mutex<SocketSet<'a>>,
    pub new_socket: Event,
    udp_binds: Mutex<HashMap<UdpBindKey, SocketHandle>>,
}

impl<'a> SocketSetWrapper<'a> {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(SocketSet::new(vec![])),
            new_socket: Event::new(),
            udp_binds: Mutex::new(HashMap::new()),
        }
    }

    pub fn add<T: AnySocket<'a>>(&self, socket: T) -> SocketHandle {
        let handle = self.inner.lock().add(socket);
        debug!("socket {}: created", handle);
        self.new_socket.notify(1);
        handle
    }

    pub fn with_socket_mut<T: AnySocket<'a>, R, F>(&self, handle: SocketHandle, f: F) -> R
    where
        F: FnOnce(&mut T) -> R,
    {
        let mut set = self.inner.lock();
        let socket = set.get_mut(handle);
        f(socket)
    }

    pub fn udp_bind(&self, handle: SocketHandle, addr: IpAddress, port: u16) -> AxResult {
        if port == 0 {
            return Ok(());
        }

        let key = UdpBindKey {
            addr: (!addr.is_unspecified()).then_some(addr),
            port,
        };
        let wildcard = UdpBindKey { addr: None, port };
        let mut binds = self.udp_binds.lock();
        if binds.contains_key(&key) || (key.addr.is_some() && binds.contains_key(&wildcard)) {
            return Err(AxError::AddrInUse);
        }
        if key.addr.is_none() && binds.keys().any(|bind| bind.port == port) {
            return Err(AxError::AddrInUse);
        }
        binds.insert(key, handle);
        Ok(())
    }

    pub fn udp_unbind(&self, handle: SocketHandle) {
        self.udp_binds
            .lock()
            .retain(|_, bound_handle| *bound_handle != handle);
    }

    pub fn remove(&self, handle: SocketHandle) {
        self.udp_unbind(handle);
        self.inner.lock().remove(handle);
        debug!("socket {}: destroyed", handle);
    }
}
