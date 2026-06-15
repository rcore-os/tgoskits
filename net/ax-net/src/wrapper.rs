use alloc::vec;

use ax_errno::{AxError, AxResult};
use ax_sync::Mutex;
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
    udp_binds: Mutex<HashMap<UdpBindKey, SocketHandle>>,
}

impl<'a> SocketSetWrapper<'a> {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(SocketSet::new(vec![])),
            udp_binds: Mutex::new(HashMap::new()),
        }
    }

    pub fn add<T: AnySocket<'a>>(&self, socket: T) -> SocketHandle {
        let handle = self.inner.lock().add(socket);
        debug!("socket {}: created", handle);
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
        let mut binds = self.udp_binds.lock();
        if !udp_bind_available(&binds, key) {
            return Err(AxError::AddrInUse);
        }
        binds.insert(key, handle);
        Ok(())
    }

    pub fn udp_port_available(&self, addr: IpAddress, port: u16) -> bool {
        if port == 0 {
            return true;
        }
        let key = UdpBindKey {
            addr: (!addr.is_unspecified()).then_some(addr),
            port,
        };
        udp_bind_available(&self.udp_binds.lock(), key)
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

fn udp_bind_available(binds: &HashMap<UdpBindKey, SocketHandle>, key: UdpBindKey) -> bool {
    let wildcard = UdpBindKey {
        addr: None,
        port: key.port,
    };
    if binds.contains_key(&key) || (key.addr.is_some() && binds.contains_key(&wildcard)) {
        return false;
    }
    key.addr.is_some() || !binds.keys().any(|bind| bind.port == key.port)
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use smoltcp::{
        iface::SocketSet,
        socket::udp,
        storage::PacketMetadata,
        wire::{IpAddress, Ipv4Address},
    };

    use super::*;

    fn key(addr: Option<Ipv4Address>, port: u16) -> UdpBindKey {
        UdpBindKey {
            addr: addr.map(IpAddress::Ipv4),
            port,
        }
    }

    fn handle() -> SocketHandle {
        let mut sockets = SocketSet::new(vec![]);
        sockets.add(udp::Socket::new(
            udp::PacketBuffer::new(vec![PacketMetadata::EMPTY; 1], vec![0; 8]),
            udp::PacketBuffer::new(vec![PacketMetadata::EMPTY; 1], vec![0; 8]),
        ))
    }

    #[test]
    fn udp_bind_rules_allow_distinct_specific_addresses() {
        let mut binds = HashMap::new();
        binds.insert(key(Some(Ipv4Address::new(192, 0, 2, 10)), 5353), handle());

        assert!(udp_bind_available(
            &binds,
            key(Some(Ipv4Address::new(198, 51, 100, 20)), 5353)
        ));
        assert!(!udp_bind_available(
            &binds,
            key(Some(Ipv4Address::new(192, 0, 2, 10)), 5353)
        ));
        assert!(!udp_bind_available(&binds, key(None, 5353)));
    }

    #[test]
    fn udp_bind_rules_reject_specific_after_wildcard() {
        let mut binds = HashMap::new();
        binds.insert(key(None, 5354), handle());

        assert!(!udp_bind_available(
            &binds,
            key(Some(Ipv4Address::new(192, 0, 2, 10)), 5354)
        ));
    }
}
