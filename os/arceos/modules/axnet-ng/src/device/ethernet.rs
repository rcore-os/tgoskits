use alloc::{
    string::String,
    sync::{Arc, Weak},
    vec,
};
use core::task::Waker;

use ax_driver::prelude::*;
use ax_sync::spin::SpinNoIrq;
use axpoll::PollSet;
use hashbrown::HashMap;
use smoltcp::{
    storage::{PacketBuffer, PacketMetadata},
    time::{Duration, Instant},
    wire::{
        ArpOperation, ArpPacket, ArpRepr, EthernetAddress, EthernetFrame, EthernetProtocol,
        EthernetRepr, IpAddress, Ipv4Cidr,
    },
};

use crate::{
    consts::{ETHERNET_MAX_PENDING_PACKETS, STANDARD_MTU},
    device::Device,
};

const EMPTY_MAC: EthernetAddress = EthernetAddress([0; 6]);
const ETHERNET_IRQ_SLOTS: usize = 16;
static IRQ_SOURCES: [SpinNoIrq<Option<Weak<EthernetIrqState>>>; ETHERNET_IRQ_SLOTS] =
    [const { SpinNoIrq::new(None) }; ETHERNET_IRQ_SLOTS];

struct Neighbor {
    hardware_address: EthernetAddress,
    expires_at: Instant,
}

struct PendingNeighbor {
    requested_at: Instant,
}

struct EthernetIrqState {
    irq_num: Option<usize>,
    driver: SpinNoIrq<AxNetDevice>,
    poll_ready: PollSet,
}

impl EthernetIrqState {
    fn handle_irq(&self) -> NetIrqEvent {
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

fn handle_ethernet_irq(slot: usize) {
    let Some(state) = IRQ_SOURCES[slot].lock().as_ref().and_then(Weak::upgrade) else {
        return;
    };

    let events = state.handle_irq();
    if events.intersects(NetIrqEvent::RX_READY | NetIrqEvent::RX_ERROR) {
        state.poll_ready.wake();
    }
}

fn handle_ethernet_irq_slot<const SLOT: usize>() {
    handle_ethernet_irq(SLOT);
}

const ETHERNET_IRQ_HANDLERS: [fn(); ETHERNET_IRQ_SLOTS] = [
    handle_ethernet_irq_slot::<0>,
    handle_ethernet_irq_slot::<1>,
    handle_ethernet_irq_slot::<2>,
    handle_ethernet_irq_slot::<3>,
    handle_ethernet_irq_slot::<4>,
    handle_ethernet_irq_slot::<5>,
    handle_ethernet_irq_slot::<6>,
    handle_ethernet_irq_slot::<7>,
    handle_ethernet_irq_slot::<8>,
    handle_ethernet_irq_slot::<9>,
    handle_ethernet_irq_slot::<10>,
    handle_ethernet_irq_slot::<11>,
    handle_ethernet_irq_slot::<12>,
    handle_ethernet_irq_slot::<13>,
    handle_ethernet_irq_slot::<14>,
    handle_ethernet_irq_slot::<15>,
];

fn reserve_ethernet_irq_slot(state: &Arc<EthernetIrqState>) -> Option<usize> {
    for (slot, source) in IRQ_SOURCES.iter().enumerate() {
        let mut source = source.lock();
        if source.as_ref().and_then(Weak::upgrade).is_none() {
            *source = Some(Arc::downgrade(state));
            return Some(slot);
        }
    }

    None
}

fn release_ethernet_irq_slot(slot: usize, state: &Arc<EthernetIrqState>) {
    let mut source = IRQ_SOURCES[slot].lock();
    if source
        .as_ref()
        .and_then(Weak::upgrade)
        .is_some_and(|registered| Arc::ptr_eq(&registered, state))
    {
        *source = None;
    }
}

impl EthernetDevice {
    const NEIGHBOR_TTL: Duration = Duration::from_secs(60);
    const ARP_REQUEST_RETRY: Duration = Duration::from_secs(1);

    pub fn new(name: String, inner: AxNetDevice, ip: Option<Ipv4Cidr>) -> Self {
        let irq_num = inner.irq_num();
        let inner = Arc::new(EthernetIrqState {
            irq_num,
            driver: SpinNoIrq::new(inner),
            poll_ready: PollSet::new(),
        });
        let pending_packets = PacketBuffer::new(
            vec![PacketMetadata::EMPTY; ETHERNET_MAX_PENDING_PACKETS],
            vec![
                0u8;
                (STANDARD_MTU + EthernetFrame::<&[u8]>::header_len())
                    * ETHERNET_MAX_PENDING_PACKETS
            ],
        );
        if let Some(irq) = inner.irq_num {
            let Some(slot) = reserve_ethernet_irq_slot(&inner) else {
                warn!("no free ethernet irq source slot for irq {irq}");
                return Self {
                    name,
                    inner,
                    neighbors: HashMap::new(),
                    pending_neighbors: HashMap::new(),
                    ip,
                    pending_packets,
                };
            };

            if !ax_hal::irq::register(irq, ETHERNET_IRQ_HANDLERS[slot]) {
                release_ethernet_irq_slot(slot, &inner);
                warn!("failed to register ethernet irq handler for irq {irq}");
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
        EthernetAddress(self.inner.driver.lock().mac_address().0)
    }

    fn send_to<F>(
        inner: &mut AxNetDevice,
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
            src_addr: EthernetAddress(inner.mac_address().0),
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
        if let Err(err) = inner.transmit(tx_buf) {
            warn!("transmit failed: {:?}", err);
        }
    }

    fn handle_frame(
        &mut self,
        frame: &[u8],
        buffer: &mut PacketBuffer<()>,
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
                    .enqueue(frame.payload().len(), ())
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
            &mut inner,
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
                    &mut inner,
                    source_hardware_addr,
                    response.buffer_len(),
                    |buf| response.emit(&mut ArpPacket::new_unchecked(buf)),
                    EthernetProtocol::Arp,
                );
            }

            if self
                .pending_packets
                .peek()
                .is_ok_and(|it| it.0 == &IpAddress::Ipv4(source_protocol_addr))
            {
                while let Ok((&next_hop, buf)) = self.pending_packets.peek() {
                    // TODO: optimize logic such that one long-pending ARP
                    // request does not block all other packets

                    let Some(neighbor) = self.neighbors.get(&next_hop) else {
                        break;
                    };
                    if neighbor.expires_at <= now {
                        // Neighbor is expired, we need to request ARP again
                        self.neighbors.remove(&next_hop);
                        let _ = self.request_arp(next_hop, now);
                        break;
                    }

                    let mut inner = self.inner.driver.lock();
                    info!(
                        "{}: sending pending IPv4 packet to {} via {}",
                        self.name, next_hop, neighbor.hardware_address
                    );
                    Self::send_to(
                        &mut inner,
                        neighbor.hardware_address,
                        buf.len(),
                        |b| b.copy_from_slice(buf),
                        EthernetProtocol::Ipv4,
                    );
                    let _ = self.pending_packets.dequeue();
                }
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
        buffer: &mut PacketBuffer<()>,
        timestamp: Instant,
        snoop: &mut dyn FnMut(&[u8]),
    ) -> bool {
        loop {
            let rx_buf = {
                let mut inner = self.inner.driver.lock();
                match inner.receive() {
                    Ok(buf) => buf,
                    Err(err) => {
                        if !matches!(err, DevError::Again) {
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

            let result = self.handle_frame(rx_buf.packet(), buffer, timestamp, snoop);
            self.inner.driver.lock().recycle_rx_buffer(rx_buf).unwrap();
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
                &mut inner,
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
                    &mut inner,
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

    fn register_waker(&self, waker: &Waker) {
        if self.inner.irq_num.is_some() {
            self.inner.poll_ready.register(waker);
        }
    }
}
