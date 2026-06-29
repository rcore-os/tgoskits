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
use core::ptr::NonNull;

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
    device::{ArpEntry, Device, EthernetDriver, NetDeviceError, NetIrqEvents},
};

const EMPTY_MAC: EthernetAddress = EthernetAddress([0; 6]);

pub trait EthernetIrqRegistration: Send + Sync {}

/// Opaque action installed into a platform IRQ registrar.
#[derive(Clone, Copy)]
pub struct EthernetIrqAction {
    data: NonNull<()>,
    handler: unsafe fn(NonNull<()>) -> EthernetIrqOutcome,
}

impl EthernetIrqAction {
    pub const fn new(
        data: NonNull<()>,
        handler: unsafe fn(NonNull<()>) -> EthernetIrqOutcome,
    ) -> Self {
        Self { data, handler }
    }

    /// Runs the IRQ action.
    ///
    /// # Safety
    ///
    /// The caller must ensure `data` still points to the Ethernet IRQ state
    /// expected by `handler`, and that the associated registration is still
    /// alive while the handler runs.
    pub unsafe fn run(self) -> EthernetIrqOutcome {
        unsafe { (self.handler)(self.data) }
    }
}

unsafe impl Send for EthernetIrqAction {}
unsafe impl Sync for EthernetIrqAction {}

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

impl EthernetIrqState {
    fn handle_irq(&self) -> NetIrqEvents {
        self.driver.lock().handle_irq()
    }
}

pub struct EthernetDevice {
    name: String,
    inner: Arc<EthernetIrqState>,
    neighbors: HashMap<IpAddress, Neighbor>,
    pending_neighbors: HashMap<IpAddress, PendingNeighbor>,
    ip: Option<Ipv4Cidr>,

    pending_packets: PacketBuffer<'static, IpAddress>,
}

unsafe fn handle_ethernet_irq(data: NonNull<()>) -> EthernetIrqOutcome {
    let state = unsafe { data.cast::<EthernetIrqState>().as_ref() };
    let events = state.handle_irq();
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
        inner: Box<dyn EthernetDriver>,
        ip: Option<Ipv4Cidr>,
        oob_rx: bool,
    ) -> Self {
        let irq = inner.irq_id();
        let mut inner = Arc::new(EthernetIrqState {
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
            let data = NonNull::from(Arc::get_mut(&mut inner).expect("new Arc is unique")).cast();
            if let Some(registrar) = ETHERNET_IRQ_REGISTRAR.get().copied() {
                let action = EthernetIrqAction::new(data, handle_ethernet_irq);
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
        }
    }

    #[inline]
    fn hardware_address(&self) -> EthernetAddress {
        EthernetAddress(self.inner.driver.lock().mac_address())
    }

    fn send_to<F>(
        inner: &mut dyn EthernetDriver,
        dst: EthernetAddress,
        size: usize,
        f: F,
        proto: EthernetProtocol,
    ) where
        F: FnOnce(&mut [u8]),
    {
        if let Err(err) = inner.recycle_tx_buffers() {
            warn!("recycle_tx_buffers failed: {:?}", err);
            return;
        }

        let repr = EthernetRepr {
            src_addr: EthernetAddress(inner.mac_address()),
            dst_addr: dst,
            ethertype: proto,
        };

        let mut tx_buf = match inner.alloc_tx_buffer(repr.buffer_len() + size) {
            Ok(buf) => buf,
            Err(err) => {
                warn!("alloc_tx_buffer failed: {:?}", err);
                return;
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
            warn!("transmit failed: {:?}", err);
        }
    }

    fn handle_frame(
        &mut self,
        frame: &[u8],
        interface_id: InterfaceId,
        buffer: &mut PacketBuffer<InterfaceId>,
        timestamp: Instant,
        snoop: &mut dyn FnMut(&[u8]),
    ) -> bool {
        let frame = EthernetFrame::new_unchecked(frame);
        let Ok(repr) = EthernetRepr::parse(&frame) else {
            warn!("Dropping malformed Ethernet frame");
            return false;
        };

        if !repr.dst_addr.is_broadcast()
            && repr.dst_addr != EMPTY_MAC
            && repr.dst_addr != self.hardware_address()
        {
            return false;
        }

        match repr.ethertype {
            EthernetProtocol::Ipv4 => {
                snoop(frame.payload());
                buffer
                    .enqueue(frame.payload().len(), interface_id)
                    .unwrap()
                    .copy_from_slice(frame.payload());
                return true;
            }
            EthernetProtocol::Arp => self.process_arp(frame.payload(), timestamp),
            _ => {}
        }

        false
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
        Self::send_to(
            &mut **inner,
            EthernetAddress::BROADCAST,
            arp_repr.buffer_len(),
            |buf| arp_repr.emit(&mut ArpPacket::new_unchecked(buf)),
            EthernetProtocol::Arp,
        );

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
                Self::send_to(
                    &mut **inner,
                    source_hardware_addr,
                    response.buffer_len(),
                    |buf| response.emit(&mut ArpPacket::new_unchecked(buf)),
                    EthernetProtocol::Arp,
                );
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
                let _ = self.pending_packets.dequeue();

                match action {
                    Action::Send(mac, payload) => {
                        let mut inner = self.inner.driver.lock();
                        info!(
                            "{}: sending pending IPv4 packet to {} via {}",
                            self.name, next_hop, mac
                        );
                        Self::send_to(
                            &mut **inner,
                            mac,
                            payload.len(),
                            |b| b.copy_from_slice(&payload),
                            EthernetProtocol::Ipv4,
                        );
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
    ) -> bool {
        loop {
            let mut rx_buf = {
                let mut inner = self.inner.driver.lock();
                match inner.receive() {
                    Ok(buf) => buf,
                    Err(err) => {
                        if !matches!(err, NetDeviceError::Again) {
                            warn!("receive failed: {:?}", err);
                        }
                        return false;
                    }
                }
            };
            trace!(
                "RECV {} bytes: {:02X?}",
                rx_buf.packet_len(),
                rx_buf.packet()
            );

            let result = self.handle_frame(rx_buf.packet(), interface_id, buffer, timestamp, snoop);
            if let Err(err) = self.inner.driver.lock().recycle_rx_buffer(&mut *rx_buf) {
                warn!("recycle_rx_buffer failed: {:?}", err);
            }
            if result {
                return true;
            }
        }
    }

    fn send(&mut self, next_hop: IpAddress, packet: &[u8], timestamp: Instant) -> bool {
        let is_subnet_broadcast =
            self.ip.and_then(|ip| ip.broadcast()).map(IpAddress::Ipv4) == Some(next_hop);
        if next_hop.is_broadcast() || is_subnet_broadcast {
            let mut inner = self.inner.driver.lock();
            Self::send_to(
                &mut **inner,
                EthernetAddress::BROADCAST,
                packet.len(),
                |buf| buf.copy_from_slice(packet),
                EthernetProtocol::Ipv4,
            );
            return false;
        }

        let need_request = match self.neighbors.get(&next_hop) {
            Some(neighbor) if neighbor.expires_at > timestamp => {
                let mut inner = self.inner.driver.lock();
                Self::send_to(
                    &mut **inner,
                    neighbor.hardware_address,
                    packet.len(),
                    |buf| buf.copy_from_slice(packet),
                    EthernetProtocol::Ipv4,
                );
                return false;
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
            return false;
        }
        if self.pending_packets.is_full() {
            warn!("Pending packets buffer is full, dropping packet");
            return false;
        }
        let Ok(dst_buffer) = self.pending_packets.enqueue(packet.len(), next_hop) else {
            warn!("Failed to enqueue packet in pending packets buffer");
            return false;
        };
        dst_buffer.copy_from_slice(packet);
        false
    }

    fn set_ipv4_addr(&mut self, addr: Option<Ipv4Cidr>) {
        self.ip = addr;
        self.neighbors.clear();
        self.pending_neighbors.clear();
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
