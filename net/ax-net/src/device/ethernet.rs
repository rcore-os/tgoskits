//! Ethernet device adapter.
//!
//! The adapter translates between the generic ax-net device contract and
//! Ethernet NIC drivers. It owns neighbor discovery state, emits Ethernet/ARP
//! frames, feeds IP packets into the router RX buffer, and exposes readiness
//! through IRQ or out-of-band wakeups.
//!
//! # Responsibilities
//!
//! - Wrap complete IP packets in Ethernet frames for TX.
//! - Parse inbound Ethernet frames, update ARP state, and deliver IP payloads
//!   to the router's RX packet buffer.
//! - Buffer a bounded number of packets while ARP resolution for a next hop is
//!   pending.
//! - Bridge platform IRQ registration into device-worker wakeups.
//!
//! # Non-Responsibilities
//!
//! The adapter does not decide which interface should be used for a destination
//! and does not inspect TCP/UDP socket state. Route selection is performed by
//! the router before Ethernet sees the packet.

use alloc::{boxed::Box, string::String, sync::Arc, vec, vec::Vec};

use ax_sync::spin::SpinNoIrq;
use axpoll::PollSet;
use hashbrown::HashMap;
use irq_framework::IrqId;
use smoltcp::{
    storage::{PacketBuffer, PacketMetadata},
    time::{Duration, Instant},
    wire::{
        ArpOperation, ArpPacket, ArpRepr, EthernetAddress, EthernetFrame, EthernetProtocol,
        EthernetRepr, IpAddress, Ipv4Cidr,
    },
};

use crate::{
    config::InterfaceId,
    consts::{ETHERNET_MAX_PENDING_PACKETS, STANDARD_MTU},
    device::{
        ArpEntry, Device, ETH_ZLEN, EthernetDriver, EthernetIrqHandler, NetDeviceError,
        NetIrqEvents,
    },
};

const EMPTY_MAC: EthernetAddress = EthernetAddress([0; 6]);

pub trait EthernetIrqRegistration: Send + Sync {}

/// Opaque action installed into a platform IRQ registrar.
pub struct EthernetIrqAction {
    handler: Box<dyn FnMut() -> EthernetIrqOutcome + Send>,
}

impl EthernetIrqAction {
    pub fn new(handler: impl FnMut() -> EthernetIrqOutcome + Send + 'static) -> Self {
        Self {
            handler: Box::new(handler),
        }
    }

    /// Runs the IRQ action.
    pub fn run(&mut self) -> EthernetIrqOutcome {
        (self.handler)()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EthernetIrqOutcome {
    /// IRQ was handled and no network worker wakeup is needed.
    Handled,
    /// IRQ indicates network progress; wake the poll path.
    Wake,
}

/// Platform hook used by Ethernet devices that expose a shared IRQ line.
pub trait EthernetIrqRegistrar: Send + Sync {
    fn register_shared(
        &self,
        name: &str,
        irq: IrqId,
        action: EthernetIrqAction,
    ) -> Result<Box<dyn EthernetIrqRegistration>, EthernetIrqRegistrationError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EthernetIrqRegistrationError {
    /// The IRQ number is invalid for this platform.
    InvalidIrq,
    /// The IRQ line cannot be shared or is already occupied.
    Busy,
    /// IRQ registration is not supported on this platform.
    Unsupported,
    /// Other platform-specific registration failure.
    Other,
}

static ETHERNET_IRQ_REGISTRAR: spin::Once<&'static dyn EthernetIrqRegistrar> = spin::Once::new();

pub fn set_ethernet_irq_registrar(registrar: &'static dyn EthernetIrqRegistrar) {
    ETHERNET_IRQ_REGISTRAR.call_once(|| registrar);
}

struct Neighbor {
    hardware_address: EthernetAddress,
    expires_at: Instant,
}

struct PendingNeighbor {
    requested_at: Instant,
}

struct EthernetIrqState {
    irq: Option<IrqId>,
    irq_registration: spin::Once<Box<dyn EthernetIrqRegistration>>,
    /// RX readiness is delivered out-of-band (outside the ethernet IRQ
    /// framework) via the device readiness poll set, e.g. an SDIO Wi-Fi chip
    /// that owns its own card interrupt and pokes the stack through
    /// `wake_net_task_irq`.
    oob_rx: bool,
    driver: SpinNoIrq<Box<dyn EthernetDriver>>,
    poll_ready: Arc<PollSet>,
}

pub struct EthernetDevice {
    name: String,
    inner: Arc<EthernetIrqState>,
    neighbors: HashMap<IpAddress, Neighbor>,
    pending_neighbors: HashMap<IpAddress, PendingNeighbor>,
    ip: Option<Ipv4Cidr>,

    pending_packets: PacketBuffer<'static, IpAddress>,
    /// Individual L2 frame lengths of packets transmitted on a side path
    /// during ARP resolution (inside `recv()`/`process_arp()`). Drained by
    /// the router's RX worker via [`Device::drain_deferred_tx`].
    deferred_tx_frame_lens: Vec<usize>,
    /// Individual L2 frame lengths of non-IP frames (ARP) received during
    /// `recv()`. These frames are processed internally and never enqueued
    /// into the IP buffer, but must still count toward RX statistics.
    /// Drained by the router's RX worker via [`Device::drain_deferred_rx`].
    deferred_rx_frame_lens: Vec<usize>,
}

fn handle_owned_ethernet_irq(handler: &mut dyn EthernetIrqHandler) -> EthernetIrqOutcome {
    ethernet_irq_outcome(handler.handle_irq())
}

fn ethernet_irq_outcome(events: NetIrqEvents) -> EthernetIrqOutcome {
    if events.intersects(NetIrqEvents::RX_READY | NetIrqEvents::RX_ERROR | NetIrqEvents::TX_DONE) {
        crate::wake_net_task_irq();
        return EthernetIrqOutcome::Wake;
    }
    EthernetIrqOutcome::Handled
}

impl EthernetDevice {
    /// Lifetime of a resolved unicast neighbour entry.  Linux uses 5 minutes
    /// for unicast neighbours; sticking to that value keeps long-running
    /// streams (e.g. a cold-start API response that takes >60 s to begin
    /// flowing) from invalidating the gateway entry mid-flow, which would
    /// otherwise force every queued ACK back into the ARP-pending buffer
    /// at once.
    const NEIGHBOR_TTL: Duration = Duration::from_secs(300);
    const ARP_REQUEST_RETRY: Duration = Duration::from_secs(1);

    /// Creates an Ethernet adapter driven by the shared IRQ/poll path.
    pub fn new(name: String, inner: Box<dyn EthernetDriver>, ip: Option<Ipv4Cidr>) -> Self {
        Self::new_inner(name, inner, ip, false)
    }

    /// Like [`new`](Self::new) but for a device whose RX readiness arrives
    /// out-of-band (via [`Device::wake_rx`]) rather than through the ethernet
    /// IRQ framework. Such a device has no IRQ registration, so `register_waker`
    /// must still arm `poll_ready` for it.
    pub fn new_oob_rx(name: String, inner: Box<dyn EthernetDriver>, ip: Option<Ipv4Cidr>) -> Self {
        Self::new_inner(name, inner, ip, true)
    }

    fn new_inner(
        name: String,
        mut inner: Box<dyn EthernetDriver>,
        ip: Option<Ipv4Cidr>,
        oob_rx: bool,
    ) -> Self {
        let irq = inner.irq_id();
        let registrar = irq.and_then(|_| ETHERNET_IRQ_REGISTRAR.get().copied());
        let irq_handler = registrar.and_then(|_| inner.take_irq_handler());
        let inner = Arc::new(EthernetIrqState {
            irq,
            irq_registration: spin::Once::new(),
            oob_rx,
            driver: SpinNoIrq::new(inner),
            poll_ready: Arc::new(PollSet::new()),
        });
        let pending_packets = PacketBuffer::new(
            vec![PacketMetadata::EMPTY; ETHERNET_MAX_PENDING_PACKETS],
            vec![
                0u8;
                (STANDARD_MTU + EthernetFrame::<&[u8]>::header_len())
                    * ETHERNET_MAX_PENDING_PACKETS
            ],
        );
        if let Some(irq) = inner.irq {
            if let Some(registrar) = registrar {
                if let Some(mut irq_handler) = irq_handler {
                    let action = EthernetIrqAction::new(move || {
                        handle_owned_ethernet_irq(&mut *irq_handler)
                    });
                    match registrar.register_shared(&name, irq, action) {
                        Ok(registration) => {
                            inner.irq_registration.call_once(|| registration);
                            inner.driver.lock().enable_irq();
                        }
                        Err(err) => {
                            warn!(
                                "failed to register ethernet irq handler for {name} irq {irq:?}: \
                                 {err:?}"
                            );
                        }
                    }
                } else {
                    warn!(
                        "skip ethernet irq registration for {name} irq {irq:?}: driver did not \
                         provide an owned IRQ handler"
                    );
                }
            } else {
                warn!(
                    "ethernet irq registrar is not installed for {name} irq {irq:?}; use polling"
                );
            }
        }

        Self {
            name,
            inner,
            neighbors: HashMap::new(),
            pending_neighbors: HashMap::new(),
            ip,

            pending_packets,
            deferred_tx_frame_lens: Vec::new(),
            deferred_rx_frame_lens: Vec::new(),
        }
    }

    #[inline]
    fn hardware_address(&self) -> EthernetAddress {
        EthernetAddress(self.inner.driver.lock().mac_address())
    }

    /// Builds an Ethernet frame around `size` bytes of payload written by `f`,
    /// emits it via `inner.transmit()`, and returns the total L2 frame length
    /// (including padding to [`ETH_ZLEN`], excluding FCS) on success, or 0 on
    /// failure.
    fn send_to<F>(
        inner: &mut dyn EthernetDriver,
        dst: EthernetAddress,
        size: usize,
        f: F,
        proto: EthernetProtocol,
    ) -> usize
    where
        F: FnOnce(&mut [u8]),
    {
        if let Err(err) = inner.recycle_tx_buffers() {
            warn!(
                "{}: recycle_tx_buffers failed: {:?}",
                inner.device_name(),
                err
            );
            return 0;
        }

        let repr = EthernetRepr {
            src_addr: EthernetAddress(inner.mac_address()),
            dst_addr: dst,
            ethertype: proto,
        };

        let total_frame_len = repr.buffer_len() + size;
        // Drivers pad short frames to ETH_ZLEN (60 bytes) in transmit(). The
        // returned length reflects the actual on-wire frame length excluding
        // FCS, aligned with Linux /proc/net/dev semantics.
        let wire_len = total_frame_len.max(ETH_ZLEN);

        let mut tx_buf = match inner.alloc_tx_buffer(total_frame_len) {
            Ok(buf) => buf,
            Err(err) => {
                warn!("{}: alloc_tx_buffer failed: {:?}", inner.device_name(), err);
                return 0;
            }
        };
        let mut frame = EthernetFrame::new_unchecked(tx_buf.packet_mut());
        repr.emit(&mut frame);
        f(frame.payload_mut());
        trace!(
            "SEND {} bytes: {:02X?}",
            tx_buf.packet_len(),
            tx_buf.packet()
        );
        if let Err(err) = inner.transmit(&mut *tx_buf) {
            warn!("{}: transmit failed: {:?}", inner.device_name(), err);
            0
        } else {
            wire_len
        }
    }

    /// Parses and handles a single Ethernet frame.
    ///
    /// Returns the raw Ethernet frame length (excluding FCS) for IP packets
    /// delivered into `buffer`, or 0 for non-IP frames (ARP, unknown
    /// EtherType), malformed frames, or frames not addressed to this device.
    fn handle_frame(
        &mut self,
        frame: &[u8],
        interface_id: InterfaceId,
        buffer: &mut PacketBuffer<InterfaceId>,
        timestamp: Instant,
        snoop: &mut dyn FnMut(&[u8]),
    ) -> usize {
        let frame_len = frame.len();
        let frame = EthernetFrame::new_unchecked(frame);
        let Ok(repr) = EthernetRepr::parse(&frame) else {
            warn!("Dropping malformed Ethernet frame");
            return 0;
        };

        if !repr.dst_addr.is_broadcast()
            && repr.dst_addr != EMPTY_MAC
            && repr.dst_addr != self.hardware_address()
        {
            return 0;
        }

        match repr.ethertype {
            EthernetProtocol::Ipv4 => {
                snoop(frame.payload());
                buffer
                    .enqueue(frame.payload().len(), interface_id)
                    .expect(
                        "recv precondition: buffer checked !rx_buffer.is_full() before calling \
                         recv()",
                    )
                    .copy_from_slice(frame.payload());
                frame_len
            }
            EthernetProtocol::Arp => {
                self.process_arp(frame.payload(), timestamp);
                // ARP frames are successfully received L2 frames — record
                // their length for RX statistics even though they were not
                // enqueued into the IP buffer.
                self.deferred_rx_frame_lens.push(frame_len);
                0
            }
            _ => {
                // Any other EtherType that has already passed the L2 validity
                // and destination-MAC filter is a good frame the host received
                // from the device. Per Linux rtnl_link_stats64, rx_packets /
                // rx_bytes count every good packet received, even one that is
                // later dropped because its protocol is unsupported by the
                // stack. Record its length for RX statistics; it is not
                // enqueued into the IP buffer.
                self.deferred_rx_frame_lens.push(frame_len);
                0
            }
        }
    }

    fn request_arp(&mut self, target_ip: IpAddress, timestamp: Instant) -> bool {
        let IpAddress::Ipv4(target_ipv4) = target_ip else {
            warn!("IPv6 address ARP is not supported: {}", target_ip);
            return false;
        };
        let Some(ip) = self.ip else {
            warn!("cannot request ARP for {target_ipv4}: ethernet IPv4 is not configured");
            return false;
        };
        info!("{}: requesting ARP for {}", self.name, target_ipv4);

        let arp_repr = ArpRepr::EthernetIpv4 {
            operation: ArpOperation::Request,
            source_hardware_addr: self.hardware_address(),
            source_protocol_addr: ip.address(),
            target_hardware_addr: EMPTY_MAC,
            target_protocol_addr: target_ipv4,
        };

        let mut inner = self.inner.driver.lock();
        let arp_frame_len = Self::send_to(
            &mut **inner,
            EthernetAddress::BROADCAST,
            arp_repr.buffer_len(),
            |buf| arp_repr.emit(&mut ArpPacket::new_unchecked(buf)),
            EthernetProtocol::Arp,
        );
        if arp_frame_len == 0 {
            warn!(
                "{}: failed to send ARP request for {}",
                self.name, target_ipv4
            );
            return false;
        }
        // ARP requests are successfully transmitted L2 frames — record
        // their length so the router RX worker can count them in TX stats.
        self.deferred_tx_frame_lens.push(arp_frame_len);

        self.pending_neighbors.insert(
            target_ip,
            PendingNeighbor {
                requested_at: timestamp,
            },
        );
        true
    }

    fn process_arp(&mut self, payload: &[u8], now: Instant) {
        let Ok(repr) = ArpPacket::new_checked(payload).and_then(|packet| ArpRepr::parse(&packet))
        else {
            warn!("Dropping malformed ARP packet");
            return;
        };

        if let ArpRepr::EthernetIpv4 {
            operation,
            source_hardware_addr,
            source_protocol_addr,
            target_hardware_addr,
            target_protocol_addr,
        } = repr
        {
            let is_unicast_mac =
                target_hardware_addr != EMPTY_MAC && !target_hardware_addr.is_broadcast();
            if is_unicast_mac && self.hardware_address() != target_hardware_addr {
                // Only process packet that are for us
                return;
            }

            if let ArpOperation::Unknown(_) = operation {
                return;
            }

            if !source_hardware_addr.is_unicast()
                || source_protocol_addr.is_broadcast()
                || source_protocol_addr.is_multicast()
                || source_protocol_addr.is_unspecified()
            {
                return;
            }
            let Some(ip) = self.ip else {
                return;
            };
            if ip.address() != target_protocol_addr {
                return;
            }

            info!(
                "{}: ARP {} -> {}",
                self.name, source_protocol_addr, source_hardware_addr
            );
            self.pending_neighbors
                .remove(&IpAddress::Ipv4(source_protocol_addr));
            self.neighbors.insert(
                IpAddress::Ipv4(source_protocol_addr),
                Neighbor {
                    hardware_address: source_hardware_addr,
                    expires_at: now + Self::NEIGHBOR_TTL,
                },
            );

            if let ArpOperation::Request = operation {
                let response = ArpRepr::EthernetIpv4 {
                    operation: ArpOperation::Reply,
                    source_hardware_addr: self.hardware_address(),
                    source_protocol_addr: ip.address(),
                    target_hardware_addr: source_hardware_addr,
                    target_protocol_addr: source_protocol_addr,
                };

                let mut inner = self.inner.driver.lock();
                let arp_frame_len = Self::send_to(
                    &mut **inner,
                    source_hardware_addr,
                    response.buffer_len(),
                    |buf| response.emit(&mut ArpPacket::new_unchecked(buf)),
                    EthernetProtocol::Arp,
                );
                // ARP replies are successfully transmitted L2 frames — record
                // their length so the router RX worker can count them in TX stats.
                if arp_frame_len > 0 {
                    self.deferred_tx_frame_lens.push(arp_frame_len);
                }
            }

            // Drain every entry in the pending queue and either send it (if
            // the next-hop is now resolved) or re-queue it in arrival order.
            // Peeking the head and stopping on the first mismatch would
            // permanently block packets queued behind an unresolvable
            // next-hop (e.g. a SYN to a fake IP at the head holds back a
            // SYN to the gateway behind it).
            //
            // The kept buffer is pre-sized so the drain does not have to
            // grow it through reallocations while a high-priority ARP IRQ
            // is being processed.
            let mut kept: Vec<(IpAddress, Vec<u8>)> =
                Vec::with_capacity(ETHERNET_MAX_PENDING_PACKETS);
            for _ in 0..ETHERNET_MAX_PENDING_PACKETS {
                let Ok((&next_hop, buf)) = self.pending_packets.peek() else {
                    break;
                };
                enum Action {
                    Send(EthernetAddress, Vec<u8>),
                    Refresh(Vec<u8>),
                    Keep(Vec<u8>),
                }
                let action = match self.neighbors.get(&next_hop) {
                    Some(neighbor) if neighbor.expires_at > now => {
                        Action::Send(neighbor.hardware_address, buf.to_vec())
                    }
                    Some(_) => Action::Refresh(buf.to_vec()),
                    None => Action::Keep(buf.to_vec()),
                };
                self.pending_packets
                    .dequeue()
                    .expect("peek succeeded moments ago; dequeue must succeed");

                match action {
                    Action::Send(mac, payload) => {
                        let mut inner = self.inner.driver.lock();
                        info!(
                            "{}: sending pending IPv4 packet to {} via {}",
                            self.name, next_hop, mac
                        );
                        let payload_len = payload.len();
                        let frame_len = Self::send_to(
                            &mut **inner,
                            mac,
                            payload_len,
                            |b| b.copy_from_slice(&payload),
                            EthernetProtocol::Ipv4,
                        );
                        if frame_len > 0 {
                            self.deferred_tx_frame_lens.push(frame_len);
                        }
                    }
                    Action::Refresh(payload) => {
                        self.neighbors.remove(&next_hop);
                        let _ = self.request_arp(next_hop, now);
                        kept.push((next_hop, payload));
                    }
                    Action::Keep(payload) => {
                        kept.push((next_hop, payload));
                    }
                }
            }
            for (next_hop, payload) in kept {
                let Ok(dst) = self.pending_packets.enqueue(payload.len(), next_hop) else {
                    warn!(
                        "{}: pending buffer overflow while restoring queue entry to {}",
                        self.name, next_hop
                    );
                    break;
                };
                dst.copy_from_slice(&payload);
            }
        }
    }
}

impl Device for EthernetDevice {
    fn name(&self) -> &str {
        &self.name
    }

    fn recv(
        &mut self,
        interface_id: InterfaceId,
        buffer: &mut PacketBuffer<InterfaceId>,
        timestamp: Instant,
        snoop: &mut dyn FnMut(&[u8]),
    ) -> usize {
        loop {
            let mut rx_buf = {
                let mut inner = self.inner.driver.lock();
                match inner.receive() {
                    Ok(buf) => buf,
                    Err(err) => {
                        if !matches!(err, NetDeviceError::Again) {
                            warn!("receive failed: {:?}", err);
                        }
                        return 0;
                    }
                }
            };
            trace!(
                "RECV {} bytes: {:02X?}",
                rx_buf.packet_len(),
                rx_buf.packet()
            );

            let frame_len =
                self.handle_frame(rx_buf.packet(), interface_id, buffer, timestamp, snoop);
            if let Err(err) = self.inner.driver.lock().recycle_rx_buffer(&mut *rx_buf) {
                warn!("recycle_rx_buffer failed: {:?}", err);
            }
            if frame_len > 0 {
                return frame_len;
            }
        }
    }

    fn send(&mut self, next_hop: IpAddress, packet: &[u8], timestamp: Instant) -> usize {
        let is_subnet_broadcast =
            self.ip.and_then(|ip| ip.broadcast()).map(IpAddress::Ipv4) == Some(next_hop);
        if next_hop.is_broadcast() || is_subnet_broadcast {
            let mut inner = self.inner.driver.lock();
            return Self::send_to(
                &mut **inner,
                EthernetAddress::BROADCAST,
                packet.len(),
                |buf| buf.copy_from_slice(packet),
                EthernetProtocol::Ipv4,
            );
        }

        let need_request = match self.neighbors.get(&next_hop) {
            Some(neighbor) if neighbor.expires_at > timestamp => {
                let mut inner = self.inner.driver.lock();
                return Self::send_to(
                    &mut **inner,
                    neighbor.hardware_address,
                    packet.len(),
                    |buf| buf.copy_from_slice(packet),
                    EthernetProtocol::Ipv4,
                );
            }
            Some(_) => {
                self.neighbors.remove(&next_hop);
                true
            }
            None => self
                .pending_neighbors
                .get(&next_hop)
                .is_none_or(|pending| timestamp >= pending.requested_at + Self::ARP_REQUEST_RETRY),
        };
        if need_request && !self.request_arp(next_hop, timestamp) {
            warn!(
                "{}: ARP request failed for {}, dropping packet",
                self.name, next_hop
            );
            return 0;
        }
        if self.pending_packets.is_full() {
            warn!(
                "{}: Pending packets buffer is full, dropping packet",
                self.name
            );
            return 0;
        }
        let Ok(dst_buffer) = self.pending_packets.enqueue(packet.len(), next_hop) else {
            warn!("Failed to enqueue packet in pending packets buffer");
            return 0;
        };
        dst_buffer.copy_from_slice(packet);
        0
    }

    fn drain_deferred_tx(&mut self) -> Vec<usize> {
        core::mem::take(&mut self.deferred_tx_frame_lens)
    }

    fn drain_deferred_rx(&mut self) -> Vec<usize> {
        core::mem::take(&mut self.deferred_rx_frame_lens)
    }

    fn set_ipv4_addr(&mut self, addr: Option<Ipv4Cidr>) {
        self.ip = addr;
        self.neighbors.clear();
        self.pending_neighbors.clear();
        // The deferred TX/RX frame-length accumulators are deliberately left
        // intact. They hold L2 frames that were already successfully
        // transmitted to or received from the device before this call; those
        // are completed link-layer events. Per Linux rtnl_link_stats64,
        // interface counters are cumulative and survive routine interface
        // operations such as an IPv4 reconfiguration, so an IP context change
        // must not retract counts that the RX worker has not drained yet.
        // Neighbor/pending state above is IP-context specific and is cleared.
    }

    fn arp_entries(&self, timestamp: Instant) -> Vec<ArpEntry> {
        self.neighbors
            .iter()
            .filter_map(|(ip_addr, neighbor)| {
                if neighbor.expires_at <= timestamp {
                    return None;
                }
                let IpAddress::Ipv4(ip_addr) = ip_addr else {
                    return None;
                };
                Some(ArpEntry {
                    ip_addr: ip_addr.octets(),
                    hw_type: 1,
                    flags: 2,
                    hw_addr: neighbor.hardware_address.0,
                    device: self.name.clone(),
                })
            })
            .collect()
    }

    fn readiness_poll(&self) -> Option<Arc<PollSet>> {
        // Only expose the poll set when there is a wake source: either an IRQ
        // registration or out-of-band RX. A pure-polling device with neither
        // must not register here, or its waker would never be woken.
        if self.inner.irq_registration.get().is_some() || self.inner.oob_rx {
            Some(self.inner.poll_ready.clone())
        } else {
            None
        }
    }
}

#[cfg(test)]
mod arp_counter_tests {
    use alloc::collections::VecDeque;

    use smoltcp::wire::{Ipv4Address, Ipv4Cidr};

    use super::*;
    use crate::device::{NetDeviceResult, NetRxBuffer, NetTxBuffer};

    // ── Mock driver infrastructure ─────────────────────────────────────

    struct MockRxBuffer {
        packet: Vec<u8>,
    }

    impl NetRxBuffer for MockRxBuffer {
        fn packet(&self) -> &[u8] {
            &self.packet
        }
    }

    struct MockTxBuffer {
        packet: Vec<u8>,
    }

    impl NetTxBuffer for MockTxBuffer {
        fn packet(&self) -> &[u8] {
            &self.packet
        }

        fn packet_mut(&mut self) -> &mut [u8] {
            &mut self.packet
        }

        fn packet_len(&self) -> usize {
            self.packet.len()
        }
    }

    /// Minimal mock EthernetDriver for testing EthernetDevice ARP paths.
    struct MockEthernetDriver {
        mac: [u8; 6],
        /// Pre-canned frames returned by `receive()` in FIFO order.
        rx_frames: VecDeque<Vec<u8>>,
        /// Frames transmitted through `transmit()`, captured for inspection.
        tx_frames: Vec<Vec<u8>>,
    }

    impl MockEthernetDriver {
        fn new(mac: [u8; 6]) -> Self {
            Self {
                mac,
                rx_frames: VecDeque::new(),
                tx_frames: Vec::new(),
            }
        }

        fn enqueue_rx_frame(&mut self, frame: Vec<u8>) {
            self.rx_frames.push_back(frame);
        }
    }

    impl EthernetDriver for MockEthernetDriver {
        fn device_name(&self) -> &str {
            "mock"
        }

        fn irq_id(&self) -> Option<IrqId> {
            None
        }

        fn enable_irq(&mut self) {}

        fn disable_irq(&mut self) {}

        fn mac_address(&self) -> [u8; 6] {
            self.mac
        }

        fn alloc_tx_buffer(&mut self, size: usize) -> NetDeviceResult<Box<dyn NetTxBuffer>> {
            Ok(Box::new(MockTxBuffer {
                packet: alloc::vec![0; size],
            }))
        }

        fn recycle_tx_buffers(&mut self) -> NetDeviceResult {
            Ok(())
        }

        fn transmit(&mut self, tx_buf: &mut dyn NetTxBuffer) -> NetDeviceResult {
            self.tx_frames.push(tx_buf.packet().to_vec());
            Ok(())
        }

        fn receive(&mut self) -> NetDeviceResult<Box<dyn NetRxBuffer>> {
            self.rx_frames
                .pop_front()
                .map(|packet| Box::new(MockRxBuffer { packet }) as Box<dyn NetRxBuffer>)
                .ok_or(NetDeviceError::Again)
        }

        fn recycle_rx_buffer(&mut self, _rx_buf: &mut dyn NetRxBuffer) -> NetDeviceResult {
            Ok(())
        }

        fn handle_irq(&mut self) -> NetIrqEvents {
            NetIrqEvents::empty()
        }
    }

    // ── Helpers ────────────────────────────────────────────────────────

    const DEV_MAC: [u8; 6] = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
    const REMOTE_MAC: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];
    const DEV_IP: Ipv4Address = Ipv4Address::new(10, 0, 0, 2);
    const REMOTE_IP: Ipv4Address = Ipv4Address::new(10, 0, 0, 1);

    fn device_ip_cidr() -> Ipv4Cidr {
        Ipv4Cidr::new(DEV_IP, 24)
    }

    fn make_test_device(mock: MockEthernetDriver) -> EthernetDevice {
        EthernetDevice::new("mock0".into(), Box::new(mock), Some(device_ip_cidr()))
    }

    /// Builds a complete Ethernet frame containing an ARP packet.
    fn build_arp_frame(
        operation: ArpOperation,
        src_mac: [u8; 6],
        dst_mac: [u8; 6],
        src_ip: Ipv4Address,
        dst_ip: Ipv4Address,
        target_mac: [u8; 6],
    ) -> Vec<u8> {
        let arp_repr = ArpRepr::EthernetIpv4 {
            operation,
            source_hardware_addr: EthernetAddress(src_mac),
            source_protocol_addr: src_ip,
            target_hardware_addr: EthernetAddress(target_mac),
            target_protocol_addr: dst_ip,
        };
        let eth_repr = EthernetRepr {
            src_addr: EthernetAddress(src_mac),
            dst_addr: EthernetAddress(dst_mac),
            ethertype: EthernetProtocol::Arp,
        };

        let total_len = eth_repr.buffer_len() + arp_repr.buffer_len();
        let mut buf = alloc::vec![0u8; total_len];
        let mut frame = EthernetFrame::new_unchecked(&mut buf);
        eth_repr.emit(&mut frame);
        arp_repr.emit(&mut ArpPacket::new_unchecked(frame.payload_mut()));
        buf
    }

    fn test_packet_buffer() -> PacketBuffer<'static, InterfaceId> {
        PacketBuffer::new(vec![PacketMetadata::EMPTY; 4], vec![0u8; STANDARD_MTU * 4])
    }

    // ── ARP RX: received ARP frames are counted in drain_deferred_rx ─────

    #[test]
    fn arp_request_rx_is_counted_in_drain_deferred_rx() {
        let mut mock = MockEthernetDriver::new(DEV_MAC);
        let arp_frame = build_arp_frame(
            ArpOperation::Request,
            REMOTE_MAC,
            DEV_MAC,
            REMOTE_IP,
            DEV_IP,
            EMPTY_MAC.0,
        );
        let frame_len = arp_frame.len();
        mock.enqueue_rx_frame(arp_frame);

        let mut device = make_test_device(mock);
        let mut buffer = test_packet_buffer();
        let ts = Instant::from_millis(0);

        // recv() processes the ARP request and returns 0 (no IP packet).
        let result = device.recv(InterfaceId::new(1), &mut buffer, ts, &mut |_| {});
        assert_eq!(result, 0);

        // The ARP frame length is recorded in the async RX side-channel.
        let rx_lens = device.drain_deferred_rx();
        assert_eq!(rx_lens, &[frame_len]);

        // Second drain is empty.
        assert!(device.drain_deferred_rx().is_empty());
    }

    #[test]
    fn arp_reply_rx_is_counted_in_drain_deferred_rx() {
        let ts = Instant::from_millis(0);

        // Build a device with both a pending neighbor entry and a
        // queued ARP reply frame.
        let mut mock = MockEthernetDriver::new(DEV_MAC);
        let arp_reply = build_arp_frame(
            ArpOperation::Reply,
            REMOTE_MAC,
            DEV_MAC,
            REMOTE_IP,
            DEV_IP,
            DEV_MAC,
        );
        let frame_len = arp_reply.len();
        mock.enqueue_rx_frame(arp_reply);

        let mut device = make_test_device(mock);
        // A pending neighbor is required for process_arp() to handle the
        // reply as relevant.
        device.pending_neighbors.insert(
            IpAddress::Ipv4(REMOTE_IP),
            PendingNeighbor { requested_at: ts },
        );

        let mut buffer = test_packet_buffer();
        let result = device.recv(InterfaceId::new(1), &mut buffer, ts, &mut |_| {});
        assert_eq!(result, 0); // ARP reply is not an IP packet

        let rx_lens = device.drain_deferred_rx();
        assert_eq!(rx_lens, &[frame_len]);
    }

    // ── ARP TX: transmitted ARP frames are counted in drain_deferred_tx ──

    #[test]
    fn arp_request_tx_is_counted_in_drain_deferred_tx() {
        let mock = MockEthernetDriver::new(DEV_MAC);
        let mut device = make_test_device(mock);
        let ts = Instant::from_millis(0);

        // Sending to an unknown neighbor triggers ARP request.
        let result = device.send(IpAddress::Ipv4(REMOTE_IP), &[0u8; 64], ts);
        // Packet is queued pending ARP; send() returns 0.
        assert_eq!(result, 0);

        // The ARP request frame length should be in drain_deferred_tx.
        let tx_lens = device.drain_deferred_tx();
        assert_eq!(tx_lens.len(), 1);
        // ARP request over Ethernet: 14 (eth hdr) + 28 (ARP) = 42 bytes.
        // With ETH_ZLEN padding: max(42, 60) = 60.
        assert_eq!(tx_lens[0], 60);
    }

    #[test]
    fn arp_reply_tx_is_counted_in_drain_deferred_tx() {
        let mut mock = MockEthernetDriver::new(DEV_MAC);
        // ARP request addressed to device from remote.
        let arp_request = build_arp_frame(
            ArpOperation::Request,
            REMOTE_MAC,
            DEV_MAC,
            REMOTE_IP,
            DEV_IP,
            EMPTY_MAC.0,
        );
        mock.enqueue_rx_frame(arp_request);

        let mut device = make_test_device(mock);
        let mut buffer = test_packet_buffer();
        let ts = Instant::from_millis(0);

        // recv() processes the ARP request, which triggers an ARP reply.
        let result = device.recv(InterfaceId::new(1), &mut buffer, ts, &mut |_| {});
        assert_eq!(result, 0);

        // Both the ARP request RX and ARP reply TX should be counted.
        let rx_lens = device.drain_deferred_rx();
        assert_eq!(rx_lens.len(), 1); // ARP request RX

        let tx_lens = device.drain_deferred_tx();
        assert_eq!(tx_lens.len(), 1); // ARP reply TX
        // ARP reply over Ethernet: 14 (eth hdr) + 28 (ARP) = 42 → padded to 60.
        assert_eq!(tx_lens[0], 60);
    }

    #[test]
    fn consecutive_arp_frames_accumulate_in_drain_deferred_rx() {
        let mut mock = MockEthernetDriver::new(DEV_MAC);
        let frame1 = build_arp_frame(
            ArpOperation::Request,
            REMOTE_MAC,
            DEV_MAC,
            REMOTE_IP,
            DEV_IP,
            EMPTY_MAC.0,
        );
        let frame2 = build_arp_frame(
            ArpOperation::Request,
            REMOTE_MAC,
            DEV_MAC,
            Ipv4Address::new(10, 0, 0, 3),
            DEV_IP,
            EMPTY_MAC.0,
        );
        let len1 = frame1.len();
        let len2 = frame2.len();
        mock.enqueue_rx_frame(frame1);
        mock.enqueue_rx_frame(frame2);

        let mut device = make_test_device(mock);
        let mut buffer = test_packet_buffer();
        let ts = Instant::from_millis(0);

        // First recv() call processes one ARP frame then returns 0 (no IP).
        let result = device.recv(InterfaceId::new(1), &mut buffer, ts, &mut |_| {});
        assert_eq!(result, 0);

        // Both ARP frame lengths should be accumulated.
        let rx_lens = device.drain_deferred_rx();
        assert_eq!(rx_lens, &[len1, len2]);

        // Drain clears the accumulator.
        assert!(device.drain_deferred_rx().is_empty());
    }

    // ── set_ipv4_addr preserves undrained frame length accumulators ───────

    /// Verifies that set_ipv4_addr() does NOT clear deferred TX/RX frame
    /// length accumulators. Per Linux rtnl_link_stats64, tx_packets counts
    /// frames successfully transmitted to the device, and IP reconfiguration
    /// cannot retract those events. If the RX worker has not yet drained
    /// deferred_tx_frame_lens after a successful ARP TX, those lengths must
    /// still be available after set_ipv4_addr() so the worker can count them.
    #[test]
    fn set_ipv4_addr_preserves_undrained_frame_lens() {
        let mock = MockEthernetDriver::new(DEV_MAC);
        let mut device = make_test_device(mock);
        let ts = Instant::from_millis(0);

        // Trigger an ARP request TX by sending to an unknown neighbor.
        let result = device.send(IpAddress::Ipv4(REMOTE_IP), &[0u8; 64], ts);
        assert_eq!(result, 0); // Packet is queued pending ARP

        // The ARP request frame length is in deferred_tx_frame_lens.
        let tx_lens_before = device.drain_deferred_tx();
        assert_eq!(tx_lens_before.len(), 1);
        assert_eq!(tx_lens_before[0], 60); // ARP request padded to ETH_ZLEN

        // Simulate another ARP request before the worker drains.
        let result = device.send(
            IpAddress::Ipv4(Ipv4Address::new(10, 0, 0, 99)),
            &[0u8; 64],
            ts,
        );
        assert_eq!(result, 0);

        // Now there's one undrained ARP TX.
        assert_eq!(device.deferred_tx_frame_lens.len(), 1);

        // Runtime reconfigures the IPv4 address (e.g., DHCP renew).
        device.set_ipv4_addr(Some(Ipv4Cidr::new(Ipv4Address::new(10, 0, 0, 99), 24)));

        // The undrained ARP TX length must still be present so the RX worker
        // can drain and count it. Clearing it here would permanently lose the
        // tx_packets/tx_bytes for an event that already succeeded.
        let tx_lens_after = device.drain_deferred_tx();
        assert_eq!(tx_lens_after.len(), 1);
        assert_eq!(tx_lens_after[0], 60);
    }

    // ── Non-ARP frames are counted in drain_deferred_rx ───────────────────

    /// Verifies that valid L2 frames with an unknown EtherType (not ARP, not
    /// IPv4) are still counted in drain_deferred_rx(). Per Linux semantics,
    /// rx_packets includes all good packets received from the device, even if
    /// the protocol is unsupported and the frame is later dropped by the stack.
    #[test]
    fn unknown_ethertype_frame_is_counted_in_drain_deferred_rx() {
        let mut mock = MockEthernetDriver::new(DEV_MAC);

        // Build a frame with EtherType 0x8100 (802.1Q VLAN tag), which this
        // stack does not support. The frame is well-formed and addressed to
        // the device, so it should count as a received packet.
        let eth_repr = EthernetRepr {
            src_addr: EthernetAddress(REMOTE_MAC),
            dst_addr: EthernetAddress(DEV_MAC),
            ethertype: EthernetProtocol::Unknown(0x8100),
        };
        let payload = [0xAAu8; 46]; // 14 + 46 = 60 bytes (ETH_ZLEN)
        let mut frame_buf = alloc::vec![0u8; eth_repr.buffer_len() + payload.len()];
        let mut frame = EthernetFrame::new_unchecked(&mut frame_buf);
        eth_repr.emit(&mut frame);
        frame.payload_mut().copy_from_slice(&payload);
        let frame_len = frame_buf.len();

        mock.enqueue_rx_frame(frame_buf);

        let mut device = make_test_device(mock);
        let mut buffer = test_packet_buffer();
        let ts = Instant::from_millis(0);

        // recv() processes the unknown frame and returns 0 (no IP packet).
        let result = device.recv(InterfaceId::new(1), &mut buffer, ts, &mut |_| {});
        assert_eq!(result, 0);

        // The frame length is recorded in the RX side-channel.
        let rx_lens = device.drain_deferred_rx();
        assert_eq!(rx_lens, &[frame_len]);
    }

    // ── ETH_ZLEN boundary test for send_to() wire_len ──────────────────

    /// Verifies that `send_to()` pads short frames to ETH_ZLEN (60 bytes)
    /// and returns the actual frame length for longer payloads. Covers
    /// below-ETH_ZLEN (0), at-ETH_ZLEN (46), and above-ETH_ZLEN (100).
    #[test]
    fn send_to_wire_len_respects_eth_zlen_padding() {
        let dst = EthernetAddress(REMOTE_MAC);

        // 0-byte payload: 14 + 0 = 14 → padded to 60.
        let mut mock = MockEthernetDriver::new(DEV_MAC);
        let wire_len =
            EthernetDevice::send_to(&mut mock, dst, 0, |_buf| {}, EthernetProtocol::Ipv4);
        assert_eq!(wire_len, 60);

        // 46-byte payload: 14 + 46 = 60 → exactly at ETH_ZLEN, no padding needed.
        let mut mock = MockEthernetDriver::new(DEV_MAC);
        let wire_len = EthernetDevice::send_to(
            &mut mock,
            dst,
            46,
            |buf| buf.copy_from_slice(&[0xAAu8; 46]),
            EthernetProtocol::Ipv4,
        );
        assert_eq!(wire_len, 60);

        // 100-byte payload: 14 + 100 = 114 → above ETH_ZLEN, no padding.
        let mut mock = MockEthernetDriver::new(DEV_MAC);
        let wire_len = EthernetDevice::send_to(
            &mut mock,
            dst,
            100,
            |buf| buf.copy_from_slice(&[0xAAu8; 100]),
            EthernetProtocol::Ipv4,
        );
        assert_eq!(wire_len, 114);
    }

    // ── Integration: combined ARP + IP recv/drain cycle ────────────────

    /// Simulates the router RX worker's inner loop: recv IP frames, drain
    /// deferred TX (ARP replies/requests), and drain deferred RX (received
    /// ARP frames). Verifies that all three counting paths produce correct
    /// byte counts in a single combined cycle.
    #[test]
    fn combined_arp_ip_recv_drain_cycle() {
        let mut mock = MockEthernetDriver::new(DEV_MAC);

        // Preload one ARP request frame addressed to the device.
        let arp_req = build_arp_frame(
            ArpOperation::Request,
            REMOTE_MAC,
            DEV_MAC,
            REMOTE_IP,
            DEV_IP,
            DEV_MAC,
        );
        mock.enqueue_rx_frame(arp_req);

        // Preload one IP frame addressed to the device.
        let eth = EthernetRepr {
            src_addr: EthernetAddress(REMOTE_MAC),
            dst_addr: EthernetAddress(DEV_MAC),
            ethertype: EthernetProtocol::Ipv4,
        };
        let ip_payload = [0x11u8; 64];
        let mut ip_frame = alloc::vec![0u8; eth.buffer_len() + ip_payload.len()];
        let mut frame = EthernetFrame::new_unchecked(&mut ip_frame);
        eth.emit(&mut frame);
        frame.payload_mut().copy_from_slice(&ip_payload);
        let expected_ip_frame_len = ip_frame.len();
        mock.enqueue_rx_frame(ip_frame);

        let mut device = make_test_device(mock);
        let mut buffer = test_packet_buffer();
        let iface = InterfaceId::new(1);

        // recv() loops internally — the ARP request is processed first
        // (returns 0, loop continues), then the IP packet is enqueued
        // and its L2 frame length is returned.
        let frame_len = device.recv(iface, &mut buffer, Instant::from_millis(0), &mut |_| {});
        assert_eq!(frame_len, expected_ip_frame_len);

        // Drain deferred RX: the received ARP request was stored.
        // RX uses the raw frame length from the driver (42 bytes); ETH_ZLEN
        // padding applies only on the TX path.
        let rx_lens = device.drain_deferred_rx();
        assert_eq!(rx_lens.len(), 1);
        assert_eq!(rx_lens[0], 42); // 14 eth hdr + 28 ARP

        // Drain deferred TX: the ARP reply that process_arp() sent.
        let tx_lens = device.drain_deferred_tx();
        assert_eq!(tx_lens.len(), 1);
        assert_eq!(tx_lens[0], 60); // 42-byte ARP reply padded to ETH_ZLEN

        // Second drain is idempotent.
        assert!(device.drain_deferred_rx().is_empty());
        assert!(device.drain_deferred_tx().is_empty());
    }
}
