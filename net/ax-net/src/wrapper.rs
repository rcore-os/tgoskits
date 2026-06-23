//! Shared smoltcp socket-set wrapper.
//!
//! ax-net keeps one global smoltcp `SocketSet` behind this wrapper. The extra
//! UDP bind table fills the per-address bind semantics that smoltcp itself does
//! not track for all POSIX cases.
//!
//! # Ownership
//!
//! All TCP, UDP, and raw smoltcp socket handles live in the same handle space.
//! This is what allows the service poller, router snooping path, listen table,
//! and orphan reaper to coordinate without per-interface socket duplication.
//!
//! # UDP Side Table
//!
//! smoltcp validates whether a UDP socket can bind, but ax-net needs
//! Linux-style wildcard/specific-address conflict checks across sockets. The
//! `udp_binds` table records only successful public binds and is cleaned when a
//! socket is removed.
//!
//! # Lock Boundary
//!
//! The wrapper lock protects smoltcp socket state. Callers should keep the lock
//! scoped to direct socket access and avoid waking tasks or acquiring the outer
//! service lock while it is held.

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
    /// `None` represents a wildcard bind.
    addr: Option<IpAddress>,
    port: u16,
}

/// Global socket container plus protocol-specific side tables.
pub(crate) struct SocketSetWrapper<'a> {
    /// The shared smoltcp socket set.
    pub inner: Mutex<SocketSet<'a>>,
    /// UDP bind ownership tracked with Linux-style wildcard conflicts.
    udp_binds: Mutex<HashMap<UdpBindKey, SocketHandle>>,
    udp_handles: Mutex<HashMap<SocketHandle, UdpBindKey>>,
}

impl<'a> SocketSetWrapper<'a> {
    /// Creates an empty wrapper around smoltcp's socket set.
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(SocketSet::new(vec![])),
            udp_binds: Mutex::new(HashMap::new()),
            udp_handles: Mutex::new(HashMap::new()),
        }
    }

    /// Adds a smoltcp socket and returns its global handle.
    pub fn add<T: AnySocket<'a>>(&self, socket: T) -> SocketHandle {
        let handle = self.inner.lock().add(socket);
        debug!("socket {}: created", handle);
        handle
    }

    /// Runs a closure with mutable access to one smoltcp socket.
    pub fn with_socket_mut<T: AnySocket<'a>, R, F>(&self, handle: SocketHandle, f: F) -> R
    where
        F: FnOnce(&mut T) -> R,
    {
        let mut set = self.inner.lock();
        let socket = set.get_mut(handle);
        f(socket)
    }

    /// Records a successful public UDP bind after checking address conflicts.
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
        self.udp_handles.lock().insert(handle, key);
        Ok(())
    }

    /// Returns whether a UDP port can be used for an ephemeral bind.
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

    /// Removes any UDP bind table entries owned by `handle`.
    pub fn udp_unbind(&self, handle: SocketHandle) {
        if let Some(key) = self.udp_handles.lock().remove(&handle) {
            self.udp_binds.lock().remove(&key);
        }
    }

    /// Removes a socket and all wrapper-maintained side-table state.
    pub fn remove(&self, handle: SocketHandle) {
        self.udp_unbind(handle);
        self.inner.lock().remove(handle);
        debug!("socket {}: destroyed", handle);
    }
}

/// Implements UDP wildcard/specific-address bind conflict rules.
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
