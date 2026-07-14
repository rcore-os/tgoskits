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

use alloc::{vec, vec::Vec};

use ax_errno::{AxError, AxResult};
use ax_sync::SpinMutex;
use hashbrown::HashMap;
use smoltcp::{
    iface::{SocketHandle, SocketSet},
    socket::AnySocket,
    wire::IpAddress,
};

use crate::addr::listen_addrs_conflict;

/// One UDP bind ownership record. Several records share a port only when every
/// binder requested SO_REUSEPORT on the identical local address, mirroring
/// Linux's reuseport group semantics.
#[derive(Clone, Debug)]
struct UdpBoundEntry {
    /// `None` represents a wildcard bind.
    addr: Option<IpAddress>,
    reuse_port: bool,
    handle: SocketHandle,
}

/// Global socket container plus protocol-specific side tables.
pub(crate) struct SocketSetWrapper<'a> {
    /// The shared smoltcp socket set.
    pub inner: SpinMutex<SocketSet<'a>>,
    /// UDP bind ownership tracked with Linux-style wildcard/reuseport conflicts.
    udp_binds: SpinMutex<HashMap<u16, Vec<UdpBoundEntry>>>,
}

impl<'a> SocketSetWrapper<'a> {
    /// Creates an empty wrapper around smoltcp's socket set.
    pub fn new() -> Self {
        Self {
            inner: SpinMutex::new(SocketSet::new(vec![])),
            udp_binds: SpinMutex::new(HashMap::new()),
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

    /// Records a public UDP bind after checking address/reuseport conflicts.
    ///
    /// A binder joins an existing group on the same port only when it and every
    /// colliding owner requested SO_REUSEPORT on the exact same local address;
    /// any other overlap is rejected with `EADDRINUSE`.
    pub fn udp_bind(
        &self,
        handle: SocketHandle,
        addr: IpAddress,
        port: u16,
        reuse_port: bool,
    ) -> AxResult {
        if port == 0 {
            return Ok(());
        }
        let addr = (!addr.is_unspecified()).then_some(addr);
        let mut binds = self.udp_binds.lock();
        let entries = binds.entry(port).or_default();
        if entries
            .iter()
            .any(|entry| udp_binds_conflict(entry, addr, reuse_port))
        {
            return Err(AxError::AddrInUse);
        }
        entries.push(UdpBoundEntry {
            addr,
            reuse_port,
            handle,
        });
        Ok(())
    }

    /// Returns whether a UDP port can be used for an ephemeral bind.
    pub fn udp_port_available(&self, addr: IpAddress, port: u16) -> bool {
        if port == 0 {
            return true;
        }
        let addr = (!addr.is_unspecified()).then_some(addr);
        match self.udp_binds.lock().get(&port) {
            None => true,
            Some(entries) => !entries
                .iter()
                .any(|entry| listen_addrs_conflict(entry.addr, addr)),
        }
    }

    /// Removes any UDP bind table entries owned by `handle`.
    pub fn udp_unbind(&self, handle: SocketHandle) {
        self.udp_binds.lock().retain(|_, entries| {
            entries.retain(|entry| entry.handle != handle);
            !entries.is_empty()
        });
    }

    /// Removes a socket and all wrapper-maintained side-table state.
    pub fn remove(&self, handle: SocketHandle) {
        self.udp_unbind(handle);
        self.inner.lock().remove(handle);
        debug!("socket {}: destroyed", handle);
    }
}

/// A new UDP bind conflicts with an existing owner unless both requested
/// SO_REUSEPORT on the exact same local address.
fn udp_binds_conflict(entry: &UdpBoundEntry, addr: Option<IpAddress>, reuse_port: bool) -> bool {
    listen_addrs_conflict(entry.addr, addr)
        && !(reuse_port && entry.reuse_port && entry.addr == addr)
}

#[cfg(test)]
mod tests {
    use alloc::{vec, vec::Vec};

    use smoltcp::{
        iface::SocketSet,
        socket::udp,
        storage::PacketMetadata,
        wire::{IpAddress, Ipv4Address},
    };

    use super::*;

    fn addr(a: u8, b: u8, c: u8, d: u8) -> IpAddress {
        IpAddress::Ipv4(Ipv4Address::new(a, b, c, d))
    }

    fn wildcard() -> IpAddress {
        addr(0, 0, 0, 0)
    }

    fn handles(n: usize) -> Vec<SocketHandle> {
        let mut sockets = SocketSet::new(vec![]);
        (0..n)
            .map(|_| {
                sockets.add(udp::Socket::new(
                    udp::PacketBuffer::new(vec![PacketMetadata::EMPTY; 1], vec![0; 8]),
                    udp::PacketBuffer::new(vec![PacketMetadata::EMPTY; 1], vec![0; 8]),
                ))
            })
            .collect()
    }

    #[test]
    fn udp_bind_rules_allow_distinct_specific_addresses() {
        let w = SocketSetWrapper::new();
        let h = handles(4);
        w.udp_bind(h[0], addr(192, 0, 2, 10), 5353, false).unwrap();
        // A different specific address on the same port is fine.
        w.udp_bind(h[1], addr(198, 51, 100, 20), 5353, false)
            .unwrap();
        // The same specific address conflicts.
        assert_eq!(
            w.udp_bind(h[2], addr(192, 0, 2, 10), 5353, false)
                .unwrap_err(),
            AxError::AddrInUse
        );
        // A wildcard bind conflicts with any existing specific bind.
        assert_eq!(
            w.udp_bind(h[3], wildcard(), 5353, false).unwrap_err(),
            AxError::AddrInUse
        );
    }

    #[test]
    fn udp_bind_rejects_specific_after_wildcard() {
        let w = SocketSetWrapper::new();
        let h = handles(2);
        w.udp_bind(h[0], wildcard(), 5354, false).unwrap();
        assert_eq!(
            w.udp_bind(h[1], addr(192, 0, 2, 10), 5354, false)
                .unwrap_err(),
            AxError::AddrInUse
        );
    }

    #[test]
    fn udp_reuseport_group_shares_a_port_while_plain_binders_conflict() {
        let w = SocketSetWrapper::new();
        let h = handles(4);
        let local = addr(127, 0, 0, 1);

        // A plain double-bind is refused.
        w.udp_bind(h[0], local, 18101, false).unwrap();
        assert_eq!(
            w.udp_bind(h[1], local, 18101, false).unwrap_err(),
            AxError::AddrInUse
        );
        // SO_REUSEPORT cannot join a group started by a non-reuseport owner.
        assert_eq!(
            w.udp_bind(h[1], local, 18101, true).unwrap_err(),
            AxError::AddrInUse
        );
        w.udp_unbind(h[0]);

        // Two reuseport binders share the port, mirroring Linux's group model.
        w.udp_bind(h[0], local, 18101, true).unwrap();
        w.udp_bind(h[1], local, 18101, true).unwrap();
        // A plain binder still cannot steal a reuseport-owned port.
        assert_eq!(
            w.udp_bind(h[2], local, 18101, false).unwrap_err(),
            AxError::AddrInUse
        );

        // Releasing one member keeps the port owned by the remaining member.
        w.udp_unbind(h[0]);
        assert_eq!(
            w.udp_bind(h[3], local, 18101, false).unwrap_err(),
            AxError::AddrInUse
        );
        // Once fully released, a plain binder may take the port.
        w.udp_unbind(h[1]);
        w.udp_bind(h[3], local, 18101, false).unwrap();
    }

    #[test]
    fn udp_port_available_avoids_any_active_bind() {
        let w = SocketSetWrapper::new();
        let h = handles(1);
        assert!(w.udp_port_available(wildcard(), 5355));
        w.udp_bind(h[0], addr(192, 0, 2, 10), 5355, false).unwrap();
        assert!(!w.udp_port_available(wildcard(), 5355));
    }
}
