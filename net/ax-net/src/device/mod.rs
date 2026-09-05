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

use alloc::{string::String, vec::Vec};
use core::ops::Range;

use smoltcp::{
    storage::PacketBuffer,
    time::Instant,
    wire::{
        IpAddress, IpProtocol, IpVersion, Ipv4Cidr, Ipv4Packet, Ipv6Packet, TcpPacket, UdpPacket,
    },
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

/// Completes a TCP or UDP checksum before software delivery.
pub(crate) fn fill_transport_checksum(packet: &mut [u8]) {
    let Ok(version) = IpVersion::of_packet(packet) else {
        return;
    };
    let (src_addr, dst_addr, protocol, transport_offset) = match version {
        IpVersion::Ipv4 => {
            let Ok(ipv4) = Ipv4Packet::new_checked(&*packet) else {
                return;
            };
            (
                IpAddress::Ipv4(ipv4.src_addr()),
                IpAddress::Ipv4(ipv4.dst_addr()),
                ipv4.next_header(),
                usize::from(ipv4.header_len()),
            )
        }
        IpVersion::Ipv6 => {
            let Ok(ipv6) = Ipv6Packet::new_checked(&*packet) else {
                return;
            };
            (
                IpAddress::Ipv6(ipv6.src_addr()),
                IpAddress::Ipv6(ipv6.dst_addr()),
                ipv6.next_header(),
                ipv6.header_len(),
            )
        }
    };
    let Some(transport) = packet.get_mut(transport_offset..) else {
        return;
    };
    match protocol {
        IpProtocol::Tcp => {
            if let Ok(mut tcp) = TcpPacket::new_checked(transport) {
                tcp.fill_checksum(&src_addr, &dst_addr);
            }
        }
        IpProtocol::Udp => {
            if let Ok(mut udp) = UdpPacket::new_checked(transport) {
                udp.fill_checksum(&src_addr, &dst_addr);
            }
        }
        _ => {}
    }
}

/// Owned IP packet whose backing RX DMA token is retained through consumption.
pub struct DeviceRxPacket {
    frame_len: usize,
    frame: ProtocolRxFrame,
    packet: Range<usize>,
}

impl DeviceRxPacket {
    pub(crate) fn with_packet_range(
        frame_len: usize,
        frame: ProtocolRxFrame,
        packet: Range<usize>,
    ) -> Self {
        assert!(packet.end <= frame.packet_len());
        Self {
            frame_len,
            frame,
            packet,
        }
    }

    /// Borrows the IP packet without releasing the RX DMA token.
    pub fn read_with<R>(&self, consume: impl FnOnce(&[u8]) -> R) -> R {
        self.frame
            .read_with(|frame| consume(&frame[self.packet.clone()]))
    }

    /// Consumes the IP packet and recycles its DMA token afterwards.
    pub fn consume<R>(self, consume: impl FnOnce(&[u8]) -> R) -> R {
        self.read_with(consume)
    }

    /// Returns the received L2 frame length excluding FCS.
    pub const fn frame_len(&self) -> usize {
        self.frame_len
    }
}

/// Result of polling a device's optional owned receive path.
pub enum DeviceRxPoll {
    /// This device only implements the compatibility receive path.
    Unsupported,
    /// The owned receive path is supported but no IP packet is ready.
    Idle,
    /// One IP packet and its backing DMA token were received.
    Packet(DeviceRxPacket),
}

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
pub trait Device: Send {
    /// Human-readable device name used in logs and userspace queries.
    fn name(&self) -> &str;

    /// Returns transport checksums the device can calculate on transmit.
    fn tx_checksum_capabilities(&self) -> TxChecksumCapabilities {
        TxChecksumCapabilities::NONE
    }

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
    /// that specific packet. The protocol executor relies on this 1:1
    /// correspondence to pair frame lengths with dequeued packets in FIFO
    /// order.
    fn recv(
        &mut self,
        interface_id: InterfaceId,
        buffer: &mut PacketBuffer<InterfaceId>,
        timestamp: Instant,
        snoop: &mut dyn FnMut(&[u8]),
    ) -> usize;

    /// Polls an optional owned receive path that retains DMA through `RxToken`.
    fn poll_owned_rx(&mut self, _timestamp: Instant) -> DeviceRxPoll {
        DeviceRxPoll::Unsupported
    }

    /// Receives directly from queue-owned backing into the final protocol
    /// destination when supported.
    ///
    /// `None` selects the compatibility [`recv`](Self::recv) path. `Some(0)`
    /// means the direct path is supported but no IP packet was delivered.
    fn recv_direct(
        &mut self,
        _timestamp: Instant,
        _deliver: &mut dyn FnMut(&[u8]) -> bool,
        _snoop: &mut dyn FnMut(&[u8]),
    ) -> Option<usize> {
        None
    }
    /// Sends a packet to the next hop.
    ///
    /// Returns the L2 frame byte count (excluding FCS) actually transmitted,
    /// or 0 if the packet was queued for later transmission (e.g. pending ARP
    /// resolution) or could not be sent. The returned byte count aligns with
    /// Linux `/proc/net/dev` semantics.
    fn send(&mut self, next_hop: IpAddress, packet: &[u8], timestamp: Instant) -> usize;

    /// Attempts a transmission while preserving transient queue backpressure.
    ///
    /// [`NetDeviceError::Again`] means the caller still owns the packet and
    /// must leave it queued until a later protocol poll.
    fn try_send(
        &mut self,
        next_hop: IpAddress,
        packet: &[u8],
        timestamp: Instant,
    ) -> NetDeviceResult<usize> {
        Ok(self.send(next_hop, packet, timestamp))
    }

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
}

#[cfg(test)]
mod tests {
    use smoltcp::wire::Ipv4Address;

    use super::*;

    struct DefaultDevice;

    impl Device for DefaultDevice {
        fn name(&self) -> &str {
            "default-device"
        }

        fn recv(
            &mut self,
            _interface_id: InterfaceId,
            _buffer: &mut PacketBuffer<InterfaceId>,
            _timestamp: Instant,
            _snoop: &mut dyn FnMut(&[u8]),
        ) -> usize {
            0
        }

        fn send(&mut self, _next_hop: IpAddress, _packet: &[u8], _timestamp: Instant) -> usize {
            0
        }
    }

    #[test]
    fn device_defaults_report_no_deferred_work_or_readiness() {
        let mut device = DefaultDevice;

        assert_eq!(device.name(), "default-device");
        assert!(device.drain_deferred_tx().is_empty());
        assert!(device.drain_deferred_rx().is_empty());
        assert_eq!(device.drain_deferred_tx_errors(), 0);
        assert_eq!(device.drain_deferred_tx_drops(), 0);
        assert_eq!(device.drain_deferred_rx_errors(), 0);
        assert_eq!(device.drain_deferred_rx_drops(), 0);
        assert!(device.arp_entries(Instant::from_millis(1)).is_empty());
        device.set_ipv4_addr(Some(Ipv4Cidr::new(Ipv4Address::LOCALHOST, 8)));
    }

    #[test]
    fn arp_entry_keeps_neighbor_metadata() {
        let entry = ArpEntry {
            ip_addr: [192, 168, 1, 1],
            hw_type: 1,
            flags: 2,
            hw_addr: [1, 2, 3, 4, 5, 6],
            device: String::from("eth0"),
        };

        assert_eq!(entry.ip_addr, [192, 168, 1, 1]);
        assert_eq!(entry.device, "eth0");
    }
}
