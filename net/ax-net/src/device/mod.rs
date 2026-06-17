//! Logical network device abstraction.
//!
//! Device implementations hide physical transport details from the single
//! protocol core. The router polls devices through this trait, while concrete
//! adapters such as Ethernet and loopback decide how packets enter or leave the
//! underlying hardware or virtual link.
//!
//! # Contract
//!
//! `recv()` moves complete IP packets into the caller-provided packet buffer;
//! `send()` accepts complete IP packets plus the already selected next hop.
//! Devices should not perform socket lookup, TCP/UDP processing, or route
//! selection. Those belong above this trait in `service` and `router`.
//!
//! # Readiness
//!
//! A device may use platform IRQs, polling, or out-of-band notifications. The
//! router asks devices for a readiness poll set and performs `PollSet`
//! register/wake operations after releasing the concrete device lock.

use alloc::{string::String, sync::Arc, vec::Vec};

use axpoll::PollSet;
use smoltcp::{
    storage::PacketBuffer,
    time::Instant,
    wire::{IpAddress, Ipv4Cidr},
};

use crate::config::InterfaceId;

mod driver;
mod ethernet;
mod loopback;
#[cfg(feature = "vsock")]
mod vsock;

pub use driver::*;
pub use ethernet::*;
pub use loopback::*;
#[cfg(feature = "vsock")]
pub use vsock::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArpEntry {
    /// IPv4 address in network byte order.
    pub ip_addr: [u8; 4],
    /// ARP hardware type.
    pub hw_type: u16,
    /// ARP entry flags exposed to userspace.
    pub flags: u16,
    /// Link-layer address.
    pub hw_addr: [u8; 6],
    /// Interface name that owns this neighbor entry.
    pub device: String,
}

/// Packet I/O endpoint behind the multi-device router.
pub trait Device: Send + Sync {
    /// Human-readable device name used in logs and userspace queries.
    fn name(&self) -> &str;

    /// Moves packets from the device into the shared IP RX buffer.
    ///
    /// Returns `true` when at least one packet was delivered and the protocol
    /// core should be polled again.
    fn recv(
        &mut self,
        interface_id: InterfaceId,
        buffer: &mut PacketBuffer<InterfaceId>,
        timestamp: Instant,
        snoop: &mut dyn FnMut(&[u8]),
    ) -> bool;
    /// Sends a packet to the next hop.
    ///
    /// Returns `true` if this operation resulted in the readiness of receive
    /// operation. This is true for loopback devices and can be used to speed
    /// up packet processing.
    fn send(&mut self, next_hop: IpAddress, packet: &[u8], timestamp: Instant) -> bool;

    /// Updates the IPv4 address used by device-local protocol helpers.
    fn set_ipv4_addr(&mut self, _addr: Option<Ipv4Cidr>) {}

    /// Returns device-local ARP/neighbor entries for userspace queries.
    fn arp_entries(&self, _timestamp: Instant) -> Vec<ArpEntry> {
        Vec::new()
    }

    /// Returns the device readiness poll set when the device has a wake source.
    ///
    /// Interrupt-driven and out-of-band devices return a poll set. Pure-polling
    /// devices should return `None`, or their wakers would sit on a poll set
    /// that is never woken.
    fn readiness_poll(&self) -> Option<Arc<PollSet>> {
        None
    }
}
