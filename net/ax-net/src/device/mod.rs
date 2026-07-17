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
    /// Returns the L2 frame byte count (excluding FCS) of the delivered IP
    /// packet, or 0 when no IP packet was enqueued. ARP and other non-IP
    /// frames are processed internally and do not produce a return value.
    ///
    /// The returned byte count aligns with Linux `/proc/net/dev` semantics
    /// (Ethernet frame without trailing FCS).
    ///
    /// # Contract
    ///
    /// Each call that returns a non-zero value MUST have enqueued exactly one
    /// IP packet into `buffer`. The return value is the L2 frame length of
    /// that specific packet. The router RX worker relies on this 1:1
    /// correspondence to pair frame lengths with dequeued packets in FIFO
    /// order.
    fn recv(
        &mut self,
        interface_id: InterfaceId,
        buffer: &mut PacketBuffer<InterfaceId>,
        timestamp: Instant,
        snoop: &mut dyn FnMut(&[u8]),
    ) -> usize;
    /// Sends a packet to the next hop.
    ///
    /// Returns the L2 frame byte count (excluding FCS) actually transmitted,
    /// or 0 if the packet was queued for later transmission (e.g. pending ARP
    /// resolution) or could not be sent. The returned byte count aligns with
    /// Linux `/proc/net/dev` semantics.
    fn send(&mut self, next_hop: IpAddress, packet: &[u8], timestamp: Instant) -> usize;

    /// Returns the per-packet L2 frame byte counts for packets transmitted
    /// on a side path during `recv()` (e.g. ARP resolution and replies)
    /// since the last call. The internal accumulator is cleared on each call.
    ///
    /// Each element is the L2 frame byte count of one packet. An empty Vec
    /// means no deferred transmissions occurred.
    fn drain_deferred_tx(&mut self) -> Vec<usize> {
        Vec::new()
    }

    /// Returns the per-packet L2 frame byte counts for non-IP frames
    /// received during `recv()` (e.g. ARP requests and replies) since the
    /// last call. The internal accumulator is cleared on each call.
    ///
    /// These frames were successfully received and processed at L2, but
    /// were not enqueued into the IP buffer. Each element is the L2 frame
    /// byte count of one received frame. An empty Vec means no non-IP
    /// frames were received.
    fn drain_deferred_rx(&mut self) -> Vec<usize> {
        Vec::new()
    }

    /// Returns the count of TX errors accumulated during device operations
    /// (e.g. buffer allocation failures, transmit hardware errors) since
    /// the last call. The internal accumulator is cleared on each call.
    fn drain_deferred_tx_errors(&mut self) -> u64 {
        0
    }

    /// Returns the count of TX drops accumulated during device operations
    /// (e.g. pending buffer full) since the last call.
    /// The internal accumulator is cleared on each call.
    ///
    /// Distinct from `drain_deferred_tx_errors`: tx_errors counts hardware/
    /// driver-level transmission failures and protocol errors; tx_drops counts
    /// packets that were intentionally discarded due to resource constraints
    /// (buffer exhaustion, queue overflow).
    fn drain_deferred_tx_drops(&mut self) -> u64 {
        0
    }

    /// Returns the count of RX errors accumulated during device operations
    /// (e.g. driver receive errors, malformed frames) since the last call.
    /// The internal accumulator is cleared on each call.
    fn drain_deferred_rx_errors(&mut self) -> u64 {
        0
    }

    /// Returns the count of RX drops accumulated during device operations
    /// (e.g. frames with unsupported EtherType that were successfully
    /// received at L2 but cannot be processed by the stack) since the last
    /// call. The internal accumulator is cleared on each call.
    fn drain_deferred_rx_drops(&mut self) -> u64 {
        0
    }

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
