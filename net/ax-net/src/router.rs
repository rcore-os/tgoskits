//! Multi-device router used as the single smoltcp device.
//!
//! ax-net exposes one smoltcp `Interface` and one global `SocketSet`, then
//! places this router underneath as a virtual device that aggregates all
//! physical and virtual links. From smoltcp's perspective this module is a
//! single `Device`; internally it performs route lookup, source-address
//! selection, loopback delivery, and handoff to protocol-side frame ports.
//!
//! # Why This Exists
//!
//! smoltcp sockets are owned by one interface. Creating one interface per NIC
//! would split socket handle spaces, make wildcard listen sockets hard to keep
//! coherent, and push routing decisions up into applications. This router keeps
//! the protocol core single-owner while still allowing multiple interfaces and
//! route metrics.
//!
//! # Data Paths
//!
//! - Queue executors replace a completed RX descriptor before publishing its
//!   old DMA token. The token remains owned through smoltcp `RxToken::consume`
//!   and then returns to the queue-local replacement cache.
//! - smoltcp TX writes into `tx_buffer`. `Router::dispatch()` parses the IP
//!   destination, selects a route, and fills a queue-owned DMA token directly.
//!   A descriptor batch shares one device notification.
//! - Loopback bypasses hardware queue domains: dispatch copies directly
//!   from TX buffer to RX buffer and asks the protocol core to poll again.
//!
//! # Concurrency Rules
//!
//! Queue executors never enter this module or take protocol locks. Route lookup,
//! device adapters, and smoltcp buffers are owned only by the protocol executor.

use alloc::{
    boxed::Box,
    collections::VecDeque,
    string::{String, ToString},
    sync::Arc,
    vec,
    vec::Vec,
};
use core::sync::atomic::{AtomicU64, Ordering};

use ax_hal::time::{NANOS_PER_MICROS, monotonic_time_nanos};
use ax_sync::SpinRwLock as RwLock;
use smoltcp::{
    iface::SocketSet,
    phy::{Checksum, DeviceCapabilities, Medium, PacketMeta},
    storage::PacketMetadata,
    time::Instant,
    wire::{
        IpAddress, IpCidr, IpProtocol, IpVersion, Ipv4Address, Ipv4Cidr, Ipv4Packet, Ipv6Packet,
        TcpPacket,
    },
};

use crate::{
    LISTEN_TABLE,
    config::{DeviceBinding, InterfaceId, RouteInfo},
    consts::{SOCKET_BUFFER_SIZE, STANDARD_MTU},
    device::{
        ArpEntry, Device, DeviceRxPacket, DeviceRxPoll, NetDeviceError, TxChecksumCapabilities,
        fill_transport_checksum,
    },
    ip_tos::apply_egress_ip_tos,
    rx_meta::packet_meta_for_rx_packet,
};

const DEVICE_RX_WORKER_BATCH: usize = 16;

/// Per-interface cumulative RX/TX byte and packet counters.
///
/// Populated from the router data paths and read by `/proc/net/dev`. Byte
/// counts use L2 frame length (IP payload plus per-device L2 framing
/// overhead, excluding trailing FCS), aligned with Linux `/proc/net/dev`
/// semantics.
#[derive(Debug, Clone)]
pub struct NetDevStats {
    pub interface_id: InterfaceId,
    pub name: String,
    pub rx_bytes: u64,
    pub rx_packets: u64,
    pub rx_errors: u64,
    pub rx_dropped: u64,
    pub tx_bytes: u64,
    pub tx_packets: u64,
    pub tx_errors: u64,
    pub tx_dropped: u64,
}

#[derive(Debug)]
pub struct Rule {
    /// Destination prefix matched by this route.
    pub filter: IpCidr,
    /// Optional gateway. `None` means the destination is directly reachable.
    pub via: Option<IpAddress>,
    /// Index into `Router::devices`.
    pub dev: usize,
    /// Stable public interface id.
    pub interface_id: InterfaceId,
    /// Source address selected when this route is used.
    pub src: IpAddress,
    /// Route metric; lower values win for equal prefix lengths.
    pub metric: u32,
    /// Insertion order used as a stable tie-breaker.
    pub order: u64,
}

impl Rule {
    /// Creates a route rule before insertion order is assigned.
    pub fn new(
        filter: IpCidr,
        via: Option<IpAddress>,
        dev: usize,
        interface_id: InterfaceId,
        src: IpAddress,
        metric: u32,
    ) -> Self {
        Self {
            filter,
            via,
            dev,
            interface_id,
            src,
            metric,
            order: 0,
        }
    }

    fn to_info(&self) -> RouteInfo {
        RouteInfo {
            filter: self.filter,
            via: self.via,
            interface_id: self.interface_id,
            source: self.src,
            metric: self.metric,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct RxMetadata {
    interface_id: InterfaceId,
    packet_meta: PacketMeta,
}

type RouterPacketBuffer = smoltcp::storage::PacketBuffer<'static, RxMetadata>;
type DevicePacketBuffer = smoltcp::storage::PacketBuffer<'static, InterfaceId>;

struct OwnedRxPacket {
    metadata: RxMetadata,
    packet: DeviceRxPacket,
}

// TX metadata is created before route lookup; dispatch() selects the real
// egress interface from the packet destination and route table.
const TX_INTERFACE_PLACEHOLDER: InterfaceId = InterfaceId::new(0);

fn rx_metadata(interface_id: InterfaceId, packet: &[u8]) -> RxMetadata {
    RxMetadata {
        interface_id,
        packet_meta: packet_meta_for_rx_packet(packet),
    }
}

fn tx_metadata() -> RxMetadata {
    RxMetadata {
        interface_id: TX_INTERFACE_PLACEHOLDER,
        packet_meta: PacketMeta::default(),
    }
}

/// Protocol-owner handle for one physical or virtual device.
struct DeviceHandle {
    /// Stable interface id exposed to the control plane.
    interface_id: InterfaceId,
    /// Device name used for logs and userspace queries.
    name: String,
    /// Concrete device implementation.
    inner: Box<dyn Device>,
    /// Bounded staging buffer used only by the unique protocol executor.
    rx_buffer: DevicePacketBuffer,
    /// Cumulative bytes/packets received on and transmitted by this interface,
    /// exposed through `/proc/net/dev`. Byte counts use L2 frame length (IP
    /// payload plus per-device L2 header), aligned with Linux semantics.
    rx_bytes: AtomicU64,
    rx_packets: AtomicU64,
    rx_errors: AtomicU64,
    rx_dropped: AtomicU64,
    tx_bytes: AtomicU64,
    tx_packets: AtomicU64,
    tx_errors: AtomicU64,
    tx_dropped: AtomicU64,
}

impl DeviceHandle {
    fn new(interface_id: InterfaceId, device: Box<dyn Device>) -> Self {
        let name = device.name().to_string();
        Self {
            interface_id,
            name,
            inner: device,
            rx_buffer: DevicePacketBuffer::new(
                vec![PacketMetadata::EMPTY; DEVICE_RX_WORKER_BATCH],
                vec![0u8; STANDARD_MTU * DEVICE_RX_WORKER_BATCH],
            ),
            rx_bytes: AtomicU64::new(0),
            rx_packets: AtomicU64::new(0),
            rx_errors: AtomicU64::new(0),
            rx_dropped: AtomicU64::new(0),
            tx_bytes: AtomicU64::new(0),
            tx_packets: AtomicU64::new(0),
            tx_errors: AtomicU64::new(0),
            tx_dropped: AtomicU64::new(0),
        }
    }

    /// Records `len` bytes received on this interface.
    ///
    /// `rx_packets` is incremented for every call regardless of `len`. Callers
    /// must ensure `len > 0` when counting a real reception; a zero `len` only
    /// makes sense for testing or diagnostic paths.
    fn count_rx(&self, len: usize) {
        // Relaxed ordering is sufficient: fetch_add provides atomic RMW that
        // guarantees no lost updates even with concurrent writers (device
        // protocol executor + loopback dispatch + deferred drains). /proc/net/dev
        // readers tolerate slight staleness, and no cross-thread
        // happens-before relationship depends on these counters.
        self.rx_bytes.fetch_add(len as u64, Ordering::Relaxed);
        self.rx_packets.fetch_add(1, Ordering::Relaxed);
    }

    /// Records `len` bytes transmitted by this interface.
    ///
    /// `tx_packets` is incremented for every call regardless of `len`. Callers
    /// must ensure `len > 0` when counting a real transmission.
    fn count_tx(&self, len: usize) {
        self.tx_bytes.fetch_add(len as u64, Ordering::Relaxed);
        self.tx_packets.fetch_add(1, Ordering::Relaxed);
    }

    fn count_rx_errors(&self, n: u64) {
        self.rx_errors.fetch_add(n, Ordering::Relaxed);
    }

    fn count_rx_dropped(&self, n: u64) {
        self.rx_dropped.fetch_add(n, Ordering::Relaxed);
    }

    fn count_tx_errors(&self, n: u64) {
        self.tx_errors.fetch_add(n, Ordering::Relaxed);
    }

    fn count_tx_dropped(&self, n: u64) {
        self.tx_dropped.fetch_add(n, Ordering::Relaxed);
    }

    fn drain_device_counters(&mut self) {
        for len in self.inner.drain_deferred_tx() {
            self.count_tx(len);
        }
        for len in self.inner.drain_deferred_rx() {
            self.count_rx(len);
        }
        let n = self.inner.drain_deferred_tx_errors();
        if n > 0 {
            self.count_tx_errors(n);
        }
        let n = self.inner.drain_deferred_tx_drops();
        if n > 0 {
            self.count_tx_dropped(n);
        }
        let n = self.inner.drain_deferred_rx_errors();
        if n > 0 {
            self.count_rx_errors(n);
        }
        let n = self.inner.drain_deferred_rx_drops();
        if n > 0 {
            self.count_rx_dropped(n);
        }
    }

    fn stats(&self) -> NetDevStats {
        NetDevStats {
            interface_id: self.interface_id,
            name: self.name.clone(),
            rx_bytes: self.rx_bytes.load(Ordering::Relaxed),
            rx_packets: self.rx_packets.load(Ordering::Relaxed),
            rx_errors: self.rx_errors.load(Ordering::Relaxed),
            rx_dropped: self.rx_dropped.load(Ordering::Relaxed),
            tx_bytes: self.tx_bytes.load(Ordering::Relaxed),
            tx_packets: self.tx_packets.load(Ordering::Relaxed),
            tx_errors: self.tx_errors.load(Ordering::Relaxed),
            tx_dropped: self.tx_dropped.load(Ordering::Relaxed),
        }
    }

    fn send(&mut self, next_hop: IpAddress, packet: &[u8], timestamp: Instant) -> bool {
        match self.try_send(next_hop, packet, timestamp) {
            Ok(consumed) => consumed,
            Err(NetDeviceError::Again) => false,
            Err(error) => {
                warn!("{}: transmit failed: {error:?}", self.name);
                self.count_tx_errors(1);
                self.drain_device_counters();
                false
            }
        }
    }

    fn try_send(
        &mut self,
        next_hop: IpAddress,
        packet: &[u8],
        timestamp: Instant,
    ) -> Result<bool, NetDeviceError> {
        if packet.len() > STANDARD_MTU {
            warn!(
                "{}: packet to {} exceeds MTU ({} bytes), dropping",
                self.name,
                next_hop,
                packet.len()
            );
            self.count_tx_dropped(1);
            return Ok(false);
        }
        let frame_len = self.inner.try_send(next_hop, packet, timestamp)?;
        if frame_len > 0 {
            self.count_tx(frame_len);
        }
        self.drain_device_counters();
        Ok(true)
    }
}

fn now() -> Instant {
    Instant::from_micros_const((monotonic_time_nanos() / NANOS_PER_MICROS) as i64)
}

#[derive(Debug, Clone, Copy)]
pub struct RouteDecision {
    /// Selected router device index.
    pub dev: usize,
    /// Selected public interface id.
    pub interface_id: InterfaceId,
    /// Source address that should be used for this route.
    pub source: IpAddress,
    /// Next hop to pass to the device.
    pub next_hop: IpAddress,
    /// Metric of the selected route.
    pub metric: u32,
}

/// Route table sorted by longest prefix, then metric, then insertion order.
pub struct RouteTable {
    rules: Vec<Rule>,
    next_order: u64,
}
impl RouteTable {
    /// Creates an empty route table.
    pub fn new() -> Self {
        Self {
            rules: Vec::new(),
            next_order: 0,
        }
    }

    /// Adds one route and re-sorts according to lookup priority.
    pub fn add_rule(&mut self, mut rule: Rule) {
        rule.order = self.next_order;
        self.next_order = self.next_order.saturating_add(1);
        self.rules.push(rule);
        self.sort_rules();
    }

    fn sort_rules(&mut self) {
        self.rules.sort_by(|a, b| {
            b.filter
                .prefix_len()
                .cmp(&a.filter.prefix_len())
                .then_with(|| a.metric.cmp(&b.metric))
                .then_with(|| a.order.cmp(&b.order))
        });
    }

    /// Selects the best route to `dst` whose interface passes `is_usable`.
    pub fn select_route_if(
        &self,
        dst: &IpAddress,
        mut is_usable: impl FnMut(InterfaceId) -> bool,
    ) -> Option<RouteDecision> {
        self.rules
            .iter()
            .find(|rule| rule.filter.contains_addr(dst) && is_usable(rule.interface_id))
            .map(|rule| RouteDecision {
                dev: rule.dev,
                interface_id: rule.interface_id,
                source: rule.src,
                next_hop: rule.via.unwrap_or(*dst),
                metric: rule.metric,
            })
    }

    /// Selects the best route to `dst` that preserves an already chosen source.
    pub fn select_route_for_source(
        &self,
        dst: &IpAddress,
        source: &IpAddress,
    ) -> Option<RouteDecision> {
        self.rules
            .iter()
            .find(|rule| rule.filter.contains_addr(dst) && &rule.src == source)
            .map(|rule| RouteDecision {
                dev: rule.dev,
                interface_id: rule.interface_id,
                source: rule.src,
                next_hop: rule.via.unwrap_or(*dst),
                metric: rule.metric,
            })
    }

    /// Returns public snapshots of IPv4 default routes.
    pub fn default_routes(&self) -> Vec<RouteInfo> {
        self.rules
            .iter()
            .filter(|rule| match rule.filter {
                IpCidr::Ipv4(cidr) => {
                    cidr.address() == Ipv4Address::UNSPECIFIED && cidr.prefix_len() == 0
                }
                _ => false,
            })
            .map(Rule::to_info)
            .collect()
    }

    /// Removes IPv4 routes owned by one interface.
    pub fn remove_ipv4_rules_for_interface(&mut self, interface_id: InterfaceId) {
        self.rules.retain(|rule| {
            !matches!(
                rule.filter,
                IpCidr::Ipv4(_) if rule.interface_id == interface_id
            )
        });
    }

    /// Atomically replaces IPv4 routes owned by one interface.
    pub fn replace_ipv4_rules_for_interface(
        &mut self,
        interface_id: InterfaceId,
        mut new_rules: Vec<Rule>,
    ) {
        self.remove_ipv4_rules_for_interface(interface_id);
        for rule in &mut new_rules {
            rule.order = self.next_order;
            self.next_order = self.next_order.saturating_add(1);
        }
        self.rules.extend(new_rules);
        self.sort_rules();
    }
}

pub(crate) type SharedRouteTable = Arc<RwLock<RouteTable>>;

/// Virtual smoltcp device that multiplexes all concrete devices.
pub struct Router {
    rx_buffer: RouterPacketBuffer,
    tx_buffer: RouterPacketBuffer,
    /// DMA-backed packets waiting for smoltcp consumption.
    ready_rx: VecDeque<OwnedRxPacket>,
    /// Checksum operations common to every registered physical TX path.
    tx_checksum_capabilities: Option<TxChecksumCapabilities>,
    devices: Vec<DeviceHandle>,
    table: SharedRouteTable,
}
impl Router {
    /// Creates the virtual multi-device endpoint used by smoltcp.
    pub fn new(table: SharedRouteTable) -> Self {
        let rx_buffer = RouterPacketBuffer::new(
            vec![PacketMetadata::EMPTY; SOCKET_BUFFER_SIZE],
            vec![0u8; STANDARD_MTU * SOCKET_BUFFER_SIZE],
        );
        let tx_buffer = RouterPacketBuffer::new(
            vec![PacketMetadata::EMPTY; SOCKET_BUFFER_SIZE],
            vec![0u8; STANDARD_MTU * SOCKET_BUFFER_SIZE],
        );
        Self {
            rx_buffer,
            tx_buffer,
            ready_rx: VecDeque::with_capacity(SOCKET_BUFFER_SIZE),
            tx_checksum_capabilities: None,
            devices: Vec::new(),
            table,
        }
    }

    /// Adds a route to the shared route table.
    pub fn add_rule(&mut self, rule: Rule) {
        self.table.write().add_rule(rule);
    }

    /// Registers a concrete device and returns its router device index.
    pub fn add_device(&mut self, interface_id: InterfaceId, device: Box<dyn Device>) -> usize {
        if interface_id != InterfaceId::LOOPBACK {
            let capabilities = device.tx_checksum_capabilities();
            self.tx_checksum_capabilities = Some(
                self.tx_checksum_capabilities
                    .map_or(capabilities, |current| current.intersection(capabilities)),
            );
        }
        self.devices.push(DeviceHandle::new(interface_id, device));
        self.devices.len() - 1
    }

    /// Returns the public interface id for a router device index.
    pub fn interface_id_for_dev(&self, dev: usize) -> Option<InterfaceId> {
        self.devices.get(dev).map(|device| device.interface_id)
    }

    /// Finds the router device index for a public interface id.
    pub fn device_index_for_interface_id(&self, interface_id: InterfaceId) -> Option<usize> {
        self.devices
            .iter()
            .position(|device| device.interface_id == interface_id)
    }

    /// Returns names of all registered devices.
    pub fn device_names(&self) -> Vec<String> {
        self.devices
            .iter()
            .map(|device| device.name.clone())
            .collect()
    }

    /// Applies an IPv4 address/gateway update to one device and its routes.
    pub fn set_ipv4_config(
        &mut self,
        dev: usize,
        interface_id: InterfaceId,
        metric: u32,
        address: Option<Ipv4Cidr>,
        gateway: Option<IpAddress>,
    ) {
        let new_rules = self.ipv4_rules(dev, interface_id, metric, address, gateway);
        self.table
            .write()
            .replace_ipv4_rules_for_interface(interface_id, new_rules);
    }

    /// Builds the connected and default IPv4 route rules for one interface.
    pub(crate) fn ipv4_rules(
        &mut self,
        dev: usize,
        interface_id: InterfaceId,
        metric: u32,
        address: Option<Ipv4Cidr>,
        gateway: Option<IpAddress>,
    ) -> Vec<Rule> {
        self.devices[dev].inner.set_ipv4_addr(address);

        let mut rules = Vec::new();
        if let Some(address) = address {
            rules.push(Rule::new(
                address.into(),
                None,
                dev,
                interface_id,
                address.address().into(),
                metric,
            ));
            if let Some(gateway) = gateway {
                rules.push(Rule::new(
                    Ipv4Cidr::new(Ipv4Address::UNSPECIFIED, 0).into(),
                    Some(gateway),
                    dev,
                    interface_id,
                    address.address().into(),
                    metric,
                ));
            }
        }
        rules
    }

    /// Moves device-produced packets into the smoltcp RX buffer.
    pub fn poll(
        &mut self,
        _timestamp: Instant,
        sockets: &mut SocketSet<'_>,
        mut snoop: impl FnMut(InterfaceId, &[u8]),
    ) -> bool {
        let mut moved_rx = false;
        let Router {
            rx_buffer,
            ready_rx,
            devices,
            ..
        } = self;
        for device in devices {
            if device.interface_id == InterfaceId::LOOPBACK {
                continue;
            }
            let mut budget = DEVICE_RX_WORKER_BATCH;
            while budget > 0 && ready_rx.len() < SOCKET_BUFFER_SIZE {
                let interface_id = device.interface_id;
                match device.inner.poll_owned_rx(now()) {
                    DeviceRxPoll::Packet(packet) => {
                        let metadata = packet.read_with(|bytes| {
                            snoop_tcp_packet(bytes, sockets);
                            snoop(interface_id, bytes);
                            rx_metadata(interface_id, bytes)
                        });
                        let frame_len = packet.frame_len();
                        ready_rx.push_back(OwnedRxPacket { metadata, packet });
                        device.count_rx(frame_len);
                        moved_rx = true;
                        budget -= 1;
                        continue;
                    }
                    DeviceRxPoll::Idle => break,
                    DeviceRxPoll::Unsupported => {}
                }
                if rx_buffer.is_full() || device.rx_buffer.is_full() {
                    break;
                }
                let mut frame_snoop = |_packet: &[u8]| {};
                let direct = device.inner.recv_direct(
                    now(),
                    &mut |packet| {
                        snoop_tcp_packet(packet, sockets);
                        snoop(interface_id, packet);
                        let Ok(dst) =
                            rx_buffer.enqueue(packet.len(), rx_metadata(interface_id, packet))
                        else {
                            return false;
                        };
                        dst.copy_from_slice(packet);
                        true
                    },
                    &mut frame_snoop,
                );
                if let Some(frame_len) = direct {
                    if frame_len == 0 {
                        break;
                    }
                    device.count_rx(frame_len);
                    moved_rx = true;
                    budget -= 1;
                    continue;
                }
                let frame_len = device.inner.recv(
                    device.interface_id,
                    &mut device.rx_buffer,
                    now(),
                    &mut frame_snoop,
                );
                if frame_len == 0 {
                    break;
                }
                let Ok((interface_id, packet)) = device.rx_buffer.dequeue() else {
                    device.count_rx_errors(1);
                    break;
                };
                snoop_tcp_packet(packet, sockets);
                snoop(interface_id, packet);
                let Ok(dst) = rx_buffer.enqueue(packet.len(), rx_metadata(interface_id, packet))
                else {
                    device.count_rx_dropped(1);
                    break;
                };
                dst.copy_from_slice(packet);
                device.count_rx(frame_len);
                moved_rx = true;
                budget -= 1;
            }
            device.drain_device_counters();
        }
        moved_rx
    }

    /// Sends a control-plane packet on a specific device.
    pub fn send_on_device(
        &mut self,
        dev: usize,
        next_hop: IpAddress,
        packet: &[u8],
        _timestamp: Instant,
    ) -> bool {
        let Router {
            rx_buffer, devices, ..
        } = self;
        let device = &mut devices[dev];
        if device.interface_id == InterfaceId::LOOPBACK {
            // Loopback traffic is transmitted and received on the same
            // interface.  Count only after successful injection so that
            // failures (buffer full, over-MTU) are correctly recorded as
            // drops rather than silently inflating the byte/packet counters.
            // The drop is attributed to rx_dropped (not tx_dropped) because
            // the packet was successfully consumed from smoltcp's TX buffer
            // and the loss occurs on the receive-side injection.  Linux
            // loopback behaves identically — send(2) returns success but the
            // packet never reaches the receiver.
            let ok =
                inject_loopback_rx_direct(rx_buffer, next_hop, packet, &mut SocketSet::new(vec![]));
            if ok {
                device.count_tx(packet.len());
                device.count_rx(packet.len());
            } else {
                device.count_rx_dropped(1);
            }
            return ok;
        }
        device.send(next_hop, packet, now())
    }

    /// Collects ARP/neighbor entries from all devices.
    pub fn arp_entries(&self, timestamp: Instant) -> Vec<ArpEntry> {
        let mut entries = Vec::new();
        for device in &self.devices {
            entries.extend(device.inner.arp_entries(timestamp));
        }
        entries
    }

    /// Returns a per-interface snapshot of RX/TX byte and packet counters.
    pub fn net_dev_stats(&self) -> Vec<NetDevStats> {
        self.devices.iter().map(|device| device.stats()).collect()
    }

    /// Device IRQs schedule queue groups directly; socket-side registration
    /// only needs to publish protocol work through the global generation.
    pub fn register_waker(&self, _binding: DeviceBinding, _waker: &core::task::Waker) {
        crate::request_poll();
    }

    /// Routes smoltcp-emitted TX packets to loopback or queue-backed frame ports.
    pub fn dispatch(&mut self, _timestamp: Instant, sockets: &mut SocketSet<'_>) -> bool {
        let mut poll_next = false;
        let Router {
            rx_buffer,
            tx_buffer,
            devices,
            table,
            ..
        } = self;
        while let Ok((_, packet)) = tx_buffer.peek() {
            let outcome = match IpVersion::of_packet(packet).expect("got invalid IP packet") {
                IpVersion::Ipv4 => {
                    let packet = smoltcp::wire::Ipv4Packet::new_checked(packet)
                        .expect("got invalid IPv4 packet");
                    let src_addr = IpAddress::Ipv4(packet.src_addr());
                    let dst_addr = IpAddress::Ipv4(packet.dst_addr());
                    if packet.dst_addr().is_broadcast() {
                        DispatchOutcome::Consumed(dispatch_link_local_fanout(
                            devices,
                            dst_addr,
                            packet.into_inner(),
                        ))
                    } else {
                        dispatch_unicast_packet(
                            rx_buffer,
                            devices,
                            table,
                            src_addr,
                            dst_addr,
                            packet.into_inner(),
                            sockets,
                        )
                    }
                }
                IpVersion::Ipv6 => {
                    let packet = smoltcp::wire::Ipv6Packet::new_checked(packet)
                        .expect("got invalid IPv6 packet");
                    let src_addr = IpAddress::Ipv6(packet.src_addr());
                    let dst_addr = IpAddress::Ipv6(packet.dst_addr());
                    if packet.dst_addr().is_multicast() {
                        DispatchOutcome::Consumed(dispatch_link_local_fanout(
                            devices,
                            dst_addr,
                            packet.into_inner(),
                        ))
                    } else {
                        dispatch_unicast_packet(
                            rx_buffer,
                            devices,
                            table,
                            src_addr,
                            dst_addr,
                            packet.into_inner(),
                            sockets,
                        )
                    }
                }
            };
            match outcome {
                DispatchOutcome::Consumed(next) => {
                    poll_next |= next;
                    tx_buffer
                        .dequeue()
                        .expect("the packet was only peeked while dispatching");
                }
                DispatchOutcome::Retry => break,
            }
        }
        poll_next
    }
}

fn dispatch_link_local_fanout(
    devices: &mut [DeviceHandle],
    dst_addr: IpAddress,
    packet: &[u8],
) -> bool {
    let mut poll_next = false;
    for dev in devices {
        if dev.interface_id != InterfaceId::LOOPBACK {
            poll_next |= dev.send(dst_addr, packet, now());
        }
    }
    poll_next
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DispatchOutcome {
    Consumed(bool),
    Retry,
}

fn dispatch_unicast_packet(
    rx_buffer: &mut RouterPacketBuffer,
    devices: &mut [DeviceHandle],
    table: &SharedRouteTable,
    src_addr: IpAddress,
    dst_addr: IpAddress,
    packet: &[u8],
    sockets: &mut SocketSet<'_>,
) -> DispatchOutcome {
    let route = {
        let routes = table.read();
        let Some(route) = routes.select_route_for_source(&dst_addr, &src_addr) else {
            debug!(
                "No route found for source {} destination {}",
                src_addr, dst_addr
            );
            // The packet is dropped at the IP layer before reaching any device's
            // ndo_start_xmit.  Linux accounts this via the system-wide SNMP counter
            // IPSTATS_MIB_OUTNOROUTES (IpOutNoRoutes in /proc/net/snmp), never via
            // per-device tx_dropped.  Once system-level SNMP counters are available
            // this should update IpOutNoRoutes instead.
            return DispatchOutcome::Consumed(false);
        };
        route
    };

    let dev = &mut devices[route.dev];
    if dev.interface_id == InterfaceId::LOOPBACK {
        // Loopback packets are copied directly from the TX buffer into the RX
        // buffer, bypassing hardware queue domains and their SPSC rings. Count
        // only after successful injection so that failures (buffer full) are
        // correctly recorded as drops rather than silently inflating the
        // byte/packet counters.
        let ok = inject_loopback_rx_direct(rx_buffer, dst_addr, packet, sockets);
        if ok {
            dev.count_tx(packet.len());
            dev.count_rx(packet.len());
        } else {
            // The packet was consumed from smoltcp's TX buffer (send(2) returns
            // success); the loss is on the receive side (buffer full or
            // over-MTU), so only rx_dropped is incremented.  Linux loopback
            // behaves identically.
            dev.count_rx_dropped(1);
        }
        DispatchOutcome::Consumed(ok)
    } else {
        match dev.try_send(route.next_hop, packet, now()) {
            Ok(consumed) => DispatchOutcome::Consumed(consumed),
            Err(NetDeviceError::Again) => DispatchOutcome::Retry,
            Err(error) => {
                warn!("{}: transmit failed: {error:?}", dev.name);
                dev.count_tx_errors(1);
                dev.drain_device_counters();
                DispatchOutcome::Consumed(false)
            }
        }
    }
}

/// Injects a loopback packet directly into the smoltcp-facing RX buffer.
fn inject_loopback_rx_direct(
    rx_buffer: &mut RouterPacketBuffer,
    dst_addr: IpAddress,
    packet: &[u8],
    sockets: &mut SocketSet<'_>,
) -> bool {
    snoop_tcp_packet(packet, sockets);
    let Ok(dst) = rx_buffer.enqueue(packet.len(), rx_metadata(InterfaceId::LOOPBACK, packet))
    else {
        warn!("Loopback: RX buffer full, dropping packet to {}", dst_addr);
        return false;
    };
    dst.copy_from_slice(packet);
    fill_transport_checksum(dst);
    true
}

/// smoltcp TX token backed by the router's temporary TX buffer.
pub struct TxToken<'a>(&'a mut RouterPacketBuffer);

impl smoltcp::phy::TxToken for TxToken<'_> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        // TX metadata is ignored: Router::dispatch parses the emitted IP
        // packet and selects the actual egress interface from the route table.
        let packet = self
            .0
            .enqueue(len, tx_metadata())
            .expect("This was checked before creating the TxToken");
        let result = f(packet);
        apply_egress_ip_tos(packet);
        result
    }
}

/// Detects passive TCP opens before smoltcp consumes the incoming packet.
fn snoop_tcp_packet(buf: &[u8], sockets: &mut SocketSet<'_>) {
    if buf.is_empty() {
        return;
    }
    let (src_addr, dst_addr, payload) = match IpVersion::of_packet(buf) {
        Ok(IpVersion::Ipv4) => {
            let Ok(packet) = Ipv4Packet::new_checked(buf) else {
                return;
            };
            if packet.next_header() != IpProtocol::Tcp {
                return;
            }
            (
                IpAddress::Ipv4(packet.src_addr()),
                IpAddress::Ipv4(packet.dst_addr()),
                packet.payload(),
            )
        }
        Ok(IpVersion::Ipv6) => {
            let Ok(packet) = Ipv6Packet::new_checked(buf) else {
                return;
            };
            if packet.next_header() != IpProtocol::Tcp {
                return;
            }
            (
                IpAddress::Ipv6(packet.src_addr()),
                IpAddress::Ipv6(packet.dst_addr()),
                packet.payload(),
            )
        }
        Err(_) => return,
    };
    let Ok(tcp_packet) = TcpPacket::new_checked(payload) else {
        return;
    };
    let src_addr = (src_addr, tcp_packet.src_port()).into();
    let dst_addr = (dst_addr, tcp_packet.dst_port()).into();
    let is_first = tcp_packet.syn() && !tcp_packet.ack();
    if is_first {
        LISTEN_TABLE.incoming_tcp_packet(src_addr, dst_addr, sockets);
    }
}

enum RxTokenPacket<'a> {
    Borrowed(&'a [u8]),
    Owned(DeviceRxPacket),
}

/// smoltcp RX token for one packet queued by the router.
pub struct RxToken<'a> {
    interface_id: InterfaceId,
    packet_meta: PacketMeta,
    packet: RxTokenPacket<'a>,
}

impl<'a> smoltcp::phy::RxToken for RxToken<'a> {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        let _ingress_if = self.interface_id;
        match self.packet {
            RxTokenPacket::Borrowed(packet) => f(packet),
            RxTokenPacket::Owned(packet) => packet.consume(f),
        }
    }

    fn meta(&self) -> PacketMeta {
        self.packet_meta
    }
}

impl smoltcp::phy::Device for Router {
    type RxToken<'a> = RxToken<'a>;
    type TxToken<'a> = TxToken<'a>;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        if self.tx_buffer.is_full() {
            return None;
        }
        let Self {
            rx_buffer,
            ready_rx,
            tx_buffer,
            ..
        } = self;
        let rx_token = if !rx_buffer.is_empty() {
            let (metadata, packet) = rx_buffer.dequeue().unwrap();
            RxToken {
                interface_id: metadata.interface_id,
                packet_meta: metadata.packet_meta,
                packet: RxTokenPacket::Borrowed(packet),
            }
        } else {
            let packet = ready_rx.pop_front()?;
            RxToken {
                interface_id: packet.metadata.interface_id,
                packet_meta: packet.metadata.packet_meta,
                packet: RxTokenPacket::Owned(packet.packet),
            }
        };
        Some((rx_token, TxToken(tx_buffer)))
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        if self.tx_buffer.is_full() {
            None
        } else {
            Some(TxToken(&mut self.tx_buffer))
        }
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ip;
        caps.max_transmission_unit = STANDARD_MTU;
        caps.max_burst_size = Some(SOCKET_BUFFER_SIZE);
        if let Some(checksum) = self.tx_checksum_capabilities {
            if checksum.supports_tcp() {
                caps.checksum.tcp = Checksum::Rx;
            }
            if checksum.supports_udp() {
                caps.checksum.udp = Checksum::Rx;
            }
        }
        caps
    }
}

#[cfg(test)]
mod tests {
    use smoltcp::storage::PacketBuffer;

    use super::*;

    const IF0: InterfaceId = InterfaceId::new(2);
    const IF1: InterfaceId = InterfaceId::new(3);
    const SRC0: IpAddress = IpAddress::Ipv4(Ipv4Address::new(10, 0, 0, 2));
    const SRC1: IpAddress = IpAddress::Ipv4(Ipv4Address::new(10, 0, 1, 2));

    struct EmptyDevice;

    impl Device for EmptyDevice {
        fn name(&self) -> &str {
            "empty"
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

    struct RetryDevice;

    impl Device for RetryDevice {
        fn name(&self) -> &str {
            "retry"
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

        fn try_send(
            &mut self,
            _next_hop: IpAddress,
            _packet: &[u8],
            _timestamp: Instant,
        ) -> crate::device::NetDeviceResult<usize> {
            Err(NetDeviceError::Again)
        }
    }

    struct ChecksumDevice;

    impl Device for ChecksumDevice {
        fn name(&self) -> &str {
            "checksum"
        }

        fn tx_checksum_capabilities(&self) -> TxChecksumCapabilities {
            TxChecksumCapabilities::TCP_UDP
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

    fn test_device_handle(device: Box<dyn Device>) -> DeviceHandle {
        DeviceHandle::new(IF0, device)
    }

    fn ipv4_cidr(addr: Ipv4Address, prefix_len: u8) -> IpCidr {
        Ipv4Cidr::new(addr, prefix_len).into()
    }

    #[test]
    fn route_lookup_uses_longest_prefix() {
        let mut table = RouteTable::new();
        table.add_rule(Rule::new(
            ipv4_cidr(Ipv4Address::UNSPECIFIED, 0),
            Some(IpAddress::Ipv4(Ipv4Address::new(10, 0, 0, 1))),
            0,
            IF0,
            SRC0,
            100,
        ));
        table.add_rule(Rule::new(
            ipv4_cidr(Ipv4Address::new(10, 0, 1, 0), 24),
            None,
            1,
            IF1,
            SRC1,
            200,
        ));

        let route = table
            .select_route_if(&IpAddress::Ipv4(Ipv4Address::new(10, 0, 1, 99)), |_| true)
            .unwrap();
        assert_eq!(route.dev, 1);
        assert_eq!(route.interface_id, IF1);
        assert_eq!(route.source, SRC1);
        assert_eq!(
            route.next_hop,
            IpAddress::Ipv4(Ipv4Address::new(10, 0, 1, 99))
        );
    }

    #[test]
    fn transient_tx_backpressure_keeps_the_router_packet_queued() {
        let table = Arc::new(RwLock::new(RouteTable::new()));
        let mut router = Router::new(Arc::clone(&table));
        router.add_device(IF0, Box::new(RetryDevice));
        router.add_rule(Rule::new(
            ipv4_cidr(Ipv4Address::UNSPECIFIED, 0),
            Some(IpAddress::Ipv4(Ipv4Address::new(10, 0, 0, 1))),
            0,
            IF0,
            SRC0,
            100,
        ));
        let packet = router
            .tx_buffer
            .enqueue(20, tx_metadata())
            .expect("the empty router TX queue has capacity");
        packet[0] = 0x45;
        packet[2..4].copy_from_slice(&20u16.to_be_bytes());
        packet[12..16].copy_from_slice(&[10, 0, 0, 2]);
        packet[16..20].copy_from_slice(&[198, 51, 100, 1]);

        let mut sockets = SocketSet::new(vec![]);
        assert!(!router.dispatch(Instant::from_millis(0), &mut sockets));
        assert_eq!(router.tx_buffer.payload_bytes_count(), 20);
        assert_eq!(router.devices[0].stats().tx_packets, 0);
        assert_eq!(router.devices[0].stats().tx_dropped, 0);
    }

    #[test]
    fn router_advertises_tx_checksum_offload_only_for_capable_devices() {
        let table = Arc::new(RwLock::new(RouteTable::new()));
        let mut router = Router::new(table);
        router.add_device(IF0, Box::new(ChecksumDevice));

        let caps = smoltcp::phy::Device::capabilities(&router);
        assert!(caps.checksum.tcp.rx());
        assert!(!caps.checksum.tcp.tx());
        assert!(caps.checksum.udp.rx());
        assert!(!caps.checksum.udp.tx());

        router.add_device(IF1, Box::new(EmptyDevice));
        let caps = smoltcp::phy::Device::capabilities(&router);
        assert!(caps.checksum.tcp.tx());
        assert!(caps.checksum.udp.tx());
    }

    #[test]
    fn route_lookup_uses_metric_for_same_prefix() {
        let mut table = RouteTable::new();
        let dst = IpAddress::Ipv4(Ipv4Address::new(203, 0, 113, 10));
        table.add_rule(Rule::new(
            ipv4_cidr(Ipv4Address::UNSPECIFIED, 0),
            Some(IpAddress::Ipv4(Ipv4Address::new(10, 0, 0, 1))),
            0,
            IF0,
            SRC0,
            200,
        ));
        table.add_rule(Rule::new(
            ipv4_cidr(Ipv4Address::UNSPECIFIED, 0),
            Some(IpAddress::Ipv4(Ipv4Address::new(10, 0, 1, 1))),
            1,
            IF1,
            SRC1,
            100,
        ));

        let route = table.select_route_if(&dst, |_| true).unwrap();
        assert_eq!(route.interface_id, IF1);
        assert_eq!(route.metric, 100);
        assert_eq!(
            route.next_hop,
            IpAddress::Ipv4(Ipv4Address::new(10, 0, 1, 1))
        );
    }

    #[test]
    fn route_lookup_keeps_stable_order_for_equal_metric() {
        let mut table = RouteTable::new();
        let dst = IpAddress::Ipv4(Ipv4Address::new(203, 0, 113, 10));
        table.add_rule(Rule::new(
            ipv4_cidr(Ipv4Address::UNSPECIFIED, 0),
            Some(IpAddress::Ipv4(Ipv4Address::new(10, 0, 0, 1))),
            0,
            IF0,
            SRC0,
            100,
        ));
        table.add_rule(Rule::new(
            ipv4_cidr(Ipv4Address::UNSPECIFIED, 0),
            Some(IpAddress::Ipv4(Ipv4Address::new(10, 0, 1, 1))),
            1,
            IF1,
            SRC1,
            100,
        ));

        let route = table.select_route_if(&dst, |_| true).unwrap();
        assert_eq!(route.interface_id, IF0);
        assert_eq!(
            route.next_hop,
            IpAddress::Ipv4(Ipv4Address::new(10, 0, 0, 1))
        );
    }

    #[test]
    fn route_lookup_skips_unusable_interface() {
        let mut table = RouteTable::new();
        let dst = IpAddress::Ipv4(Ipv4Address::new(203, 0, 113, 10));
        table.add_rule(Rule::new(
            ipv4_cidr(Ipv4Address::UNSPECIFIED, 0),
            Some(IpAddress::Ipv4(Ipv4Address::new(10, 0, 0, 1))),
            0,
            IF0,
            SRC0,
            100,
        ));
        table.add_rule(Rule::new(
            ipv4_cidr(Ipv4Address::UNSPECIFIED, 0),
            Some(IpAddress::Ipv4(Ipv4Address::new(10, 0, 1, 1))),
            1,
            IF1,
            SRC1,
            200,
        ));

        let route = table
            .select_route_if(&dst, |interface_id| interface_id != IF0)
            .unwrap();
        assert_eq!(route.interface_id, IF1);
    }

    #[test]
    fn snoop_tcp_packet_drops_truncated_ip_and_tcp_headers() {
        const IPV4_HEADER_LEN: usize = 20;
        const IPV6_HEADER_LEN: usize = 40;
        const TCP_HEADER_LEN: usize = 20;

        let mut sockets = SocketSet::new(vec![]);

        let mut ipv4_tcp = [0u8; IPV4_HEADER_LEN + TCP_HEADER_LEN];
        ipv4_tcp[0] = 0x45;
        let ipv4_tcp_len = ipv4_tcp.len() as u16;
        ipv4_tcp[2..4].copy_from_slice(&ipv4_tcp_len.to_be_bytes());
        ipv4_tcp[9] = IpProtocol::Tcp.into();
        for len in 0..ipv4_tcp.len() {
            snoop_tcp_packet(&ipv4_tcp[..len], &mut sockets);
        }

        let mut ipv6_tcp = [0u8; IPV6_HEADER_LEN + TCP_HEADER_LEN];
        ipv6_tcp[0] = 0x60;
        ipv6_tcp[4..6].copy_from_slice(&20u16.to_be_bytes());
        ipv6_tcp[6] = IpProtocol::Tcp.into();
        for len in 0..ipv6_tcp.len() {
            snoop_tcp_packet(&ipv6_tcp[..len], &mut sockets);
        }

        // Keep the IP header complete and its length fields consistent so the
        // packet reaches the TCP parser. The old unchecked TCP parser then
        // read ports from these 0-19 byte payloads and panicked.
        for tcp_len in 0..TCP_HEADER_LEN {
            let mut ipv4_tcp = vec![0u8; IPV4_HEADER_LEN + tcp_len];
            ipv4_tcp[0] = 0x45;
            let ipv4_len = ipv4_tcp.len() as u16;
            ipv4_tcp[2..4].copy_from_slice(&ipv4_len.to_be_bytes());
            ipv4_tcp[9] = IpProtocol::Tcp.into();
            snoop_tcp_packet(&ipv4_tcp, &mut sockets);

            let mut ipv6_tcp = vec![0u8; IPV6_HEADER_LEN + tcp_len];
            ipv6_tcp[0] = 0x60;
            ipv6_tcp[4..6].copy_from_slice(&(tcp_len as u16).to_be_bytes());
            ipv6_tcp[6] = IpProtocol::Tcp.into();
            snoop_tcp_packet(&ipv6_tcp, &mut sockets);
        }
    }

    #[test]
    fn default_routes_only_reports_zero_prefix_ipv4_rules() {
        let mut table = RouteTable::new();
        table.add_rule(Rule::new(
            ipv4_cidr(Ipv4Address::UNSPECIFIED, 0),
            Some(IpAddress::Ipv4(Ipv4Address::new(10, 0, 0, 1))),
            0,
            IF0,
            SRC0,
            100,
        ));
        table.add_rule(Rule::new(
            ipv4_cidr(Ipv4Address::new(10, 0, 1, 0), 24),
            None,
            1,
            IF1,
            SRC1,
            100,
        ));

        let routes = table.default_routes();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].interface_id, IF0);
    }

    /// When no route exists for a destination, `dispatch_unicast_packet`
    /// must NOT attribute the L3 drop to any interface's `tx_dropped`.
    /// Linux accounts this as the system-wide `IpOutNoRoutes` SNMP counter;
    /// per-interface tx_dropped is reserved for drops after an egress device
    /// has been selected (e.g. queue full, MTU exceeded).  This test guards
    /// against accidentally polluting interface counters via source-route
    /// fallback (the primary path the old code used).  The secondary
    /// loopback-only fallback (when the source address also has no covering
    /// route) is not exercised here — it requires a loopback device — but
    /// was removed together with the source-route path.
    #[test]
    fn no_route_does_not_count_interface_tx_dropped() {
        use smoltcp::{iface::SocketSet, storage::PacketMetadata};

        // Two devices with independent counters.
        let dev0 = test_device_handle(Box::new(EmptyDevice));
        let dev1 = DeviceHandle::new(IF1, Box::new(EmptyDevice));
        let mut devices = vec![dev0, dev1];

        // Route table: only a subnet route for dev0, which covers the
        // source address but NOT the destination.
        let mut route_table = RouteTable::new();
        route_table.add_rule(Rule::new(
            ipv4_cidr(Ipv4Address::new(10, 0, 0, 0), 24),
            Some(SRC0),
            0,
            IF0, // dev index in `devices`
            SRC0,
            100,
        ));
        let shared_table: SharedRouteTable = Arc::new(RwLock::new(route_table));

        let mut rx_buffer: RouterPacketBuffer = PacketBuffer::new(
            vec![PacketMetadata::EMPTY; 1],
            vec![0u8; super::STANDARD_MTU],
        );
        let mut sockets = SocketSet::new(vec![]);

        let src_addr = SRC0;
        let dst_addr = IpAddress::Ipv4(Ipv4Address::new(203, 0, 113, 10));
        let packet = [0u8; 64];

        let before: Vec<_> = devices.iter().map(|d| d.stats()).collect();

        let outcome = dispatch_unicast_packet(
            &mut rx_buffer,
            &mut devices,
            &shared_table,
            src_addr,
            dst_addr,
            &packet,
            &mut sockets,
        );

        assert_eq!(
            outcome,
            DispatchOutcome::Consumed(false),
            "no-route dispatch must consume the packet without scheduling work"
        );

        for (i, dev) in devices.iter().enumerate() {
            let snap = dev.stats();
            assert_eq!(
                snap.tx_dropped, before[i].tx_dropped,
                "device {i} tx_dropped changed from {} to {} after no-route dispatch",
                before[i].tx_dropped, snap.tx_dropped,
            );
        }
    }
}

#[cfg(test)]
mod l2_counter_tests {
    use smoltcp::{
        storage::{PacketBuffer, PacketMetadata},
        time::Instant,
        wire::{IpAddress, Ipv4Address},
    };

    use super::*;

    const IF0: InterfaceId = InterfaceId::new(2);

    /// Configurable mock device for L2 frame-length counter tests.
    struct CountingMockDevice {
        name: &'static str,
        send_returns: usize,
        recv_returns: usize,
        /// Pre-canned lengths returned by drain_deferred_tx(), drained on each call.
        deferred_tx_lens: Vec<usize>,
        /// Pre-canned lengths returned by drain_deferred_rx(), drained on each call.
        deferred_rx_lens: Vec<usize>,
    }

    impl Device for CountingMockDevice {
        fn name(&self) -> &str {
            self.name
        }

        fn recv(
            &mut self,
            _interface_id: InterfaceId,
            _buffer: &mut PacketBuffer<InterfaceId>,
            _timestamp: Instant,
            _snoop: &mut dyn FnMut(&[u8]),
        ) -> usize {
            self.recv_returns
        }

        fn send(&mut self, _next_hop: IpAddress, _packet: &[u8], _timestamp: Instant) -> usize {
            self.send_returns
        }

        fn drain_deferred_tx(&mut self) -> Vec<usize> {
            core::mem::take(&mut self.deferred_tx_lens)
        }

        fn drain_deferred_rx(&mut self) -> Vec<usize> {
            core::mem::take(&mut self.deferred_rx_lens)
        }
    }

    fn test_device_handle(device: Box<dyn Device>) -> DeviceHandle {
        DeviceHandle::new(IF0, device)
    }

    fn test_ip() -> IpAddress {
        IpAddress::Ipv4(Ipv4Address::new(10, 0, 0, 1))
    }

    fn test_packet_buffer() -> PacketBuffer<'static, InterfaceId> {
        PacketBuffer::new(
            vec![PacketMetadata::EMPTY; 1],
            vec![0u8; super::STANDARD_MTU],
        )
    }

    // ── count_rx / count_tx ────────────────────────────────────────────

    #[test]
    fn count_rx_accumulates_bytes_and_packets() {
        let device = test_device_handle(Box::new(CountingMockDevice {
            name: "mock",
            send_returns: 0,
            deferred_tx_lens: vec![],
            deferred_rx_lens: vec![],
            recv_returns: 0,
        }));

        device.count_rx(100);
        assert_eq!(device.stats().rx_bytes, 100);
        assert_eq!(device.stats().rx_packets, 1);

        device.count_rx(200);
        assert_eq!(device.stats().rx_bytes, 300);
        assert_eq!(device.stats().rx_packets, 2);
    }

    #[test]
    fn count_tx_accumulates_bytes_and_packets() {
        let device = test_device_handle(Box::new(CountingMockDevice {
            name: "mock",
            send_returns: 0,
            deferred_tx_lens: vec![],
            deferred_rx_lens: vec![],
            recv_returns: 0,
        }));

        device.count_tx(64);
        assert_eq!(device.stats().tx_bytes, 64);
        assert_eq!(device.stats().tx_packets, 1);

        device.count_tx(1500);
        assert_eq!(device.stats().tx_bytes, 1564);
        assert_eq!(device.stats().tx_packets, 2);
    }

    // ── stats snapshot ─────────────────────────────────────────────────

    #[test]
    fn stats_starts_at_zero() {
        let device = test_device_handle(Box::new(CountingMockDevice {
            name: "mock",
            send_returns: 0,
            deferred_tx_lens: vec![],
            deferred_rx_lens: vec![],
            recv_returns: 0,
        }));

        let snap = device.stats();
        assert_eq!(snap.rx_bytes, 0);
        assert_eq!(snap.rx_packets, 0);
        assert_eq!(snap.tx_bytes, 0);
        assert_eq!(snap.tx_packets, 0);
    }

    #[test]
    fn stats_reflects_current_counters_after_counting() {
        let device = test_device_handle(Box::new(CountingMockDevice {
            name: "mock",
            send_returns: 0,
            deferred_tx_lens: vec![],
            deferred_rx_lens: vec![],
            recv_returns: 0,
        }));

        device.count_rx(100);
        device.count_tx(64);

        let snap = device.stats();
        assert_eq!(snap.rx_bytes, 100);
        assert_eq!(snap.rx_packets, 1);
        assert_eq!(snap.tx_bytes, 64);
        assert_eq!(snap.tx_packets, 1);
    }

    // ── frame-length contract: send ────────────────────────────────────

    #[test]
    fn send_returns_frame_len_tx_counts_l2_not_ip_payload() {
        let mut device = test_device_handle(Box::new(CountingMockDevice {
            name: "mock",
            send_returns: 1514, // L2 frame length (14 eth hdr + 1500 IP payload)
            deferred_tx_lens: vec![],
            deferred_rx_lens: vec![],
            recv_returns: 0,
        }));

        // Simulate the protocol executor's TX accounting step.
        let frame_len = device
            .inner
            .send(test_ip(), &[0u8; 100], Instant::from_millis(0));
        assert_eq!(frame_len, 1514);
        if frame_len > 0 {
            device.count_tx(frame_len);
        }

        let snap = device.stats();
        // Byte counter reflects L2 frame length, NOT the IP payload (100 bytes)
        assert_eq!(snap.tx_bytes, 1514);
        assert_eq!(snap.tx_packets, 1);
    }

    #[test]
    fn send_returns_zero_no_tx_counted() {
        let mut device = test_device_handle(Box::new(CountingMockDevice {
            name: "mock",
            send_returns: 0, // ARP pending or send failure
            deferred_tx_lens: vec![],
            deferred_rx_lens: vec![],
            recv_returns: 0,
        }));

        let frame_len = device
            .inner
            .send(test_ip(), &[0u8; 100], Instant::from_millis(0));
        assert_eq!(frame_len, 0);
        // Worker skips count_tx when frame_len == 0
        if frame_len > 0 {
            device.count_tx(frame_len);
        }

        let snap = device.stats();
        assert_eq!(snap.tx_bytes, 0);
        assert_eq!(snap.tx_packets, 0);
    }

    // ── frame-length contract: recv ────────────────────────────────────

    #[test]
    fn recv_returns_frame_len_rx_counts_it() {
        let mut device = test_device_handle(Box::new(CountingMockDevice {
            name: "mock",
            send_returns: 0,
            deferred_tx_lens: vec![],
            deferred_rx_lens: vec![],
            recv_returns: 1514,
        }));

        let frame_len = device.inner.recv(
            IF0,
            &mut test_packet_buffer(),
            Instant::from_millis(0),
            &mut |_| {},
        );
        assert_eq!(frame_len, 1514);
        if frame_len > 0 {
            device.count_rx(frame_len);
        }

        let snap = device.stats();
        assert_eq!(snap.rx_bytes, 1514);
        assert_eq!(snap.rx_packets, 1);
    }

    #[test]
    fn recv_returns_zero_no_rx_counted() {
        let mut device = test_device_handle(Box::new(CountingMockDevice {
            name: "mock",
            send_returns: 0,
            deferred_tx_lens: vec![],
            deferred_rx_lens: vec![],
            recv_returns: 0, // no packet available
        }));

        let frame_len = device.inner.recv(
            IF0,
            &mut test_packet_buffer(),
            Instant::from_millis(0),
            &mut |_| {},
        );
        assert_eq!(frame_len, 0);
        if frame_len > 0 {
            device.count_rx(frame_len);
        }

        let snap = device.stats();
        assert_eq!(snap.rx_bytes, 0);
        assert_eq!(snap.rx_packets, 0);
    }

    // ── drain_deferred_tx default ─────────────────────────────────────────

    #[test]
    fn drain_deferred_tx_default_returns_empty_vec() {
        let mut device = CountingMockDevice {
            name: "mock",
            send_returns: 0,
            deferred_tx_lens: vec![],
            deferred_rx_lens: vec![],
            recv_returns: 0,
        };

        // Default trait implementation returns Vec::new().
        let drained = device.drain_deferred_tx();
        assert!(drained.is_empty());

        // Second call is also empty (no side effects).
        let drained = device.drain_deferred_tx();
        assert!(drained.is_empty());
    }

    // ── drain_deferred_rx default ─────────────────────────────────────────

    #[test]
    fn drain_deferred_rx_default_returns_empty_vec() {
        let mut device = CountingMockDevice {
            name: "mock",
            send_returns: 0,
            deferred_tx_lens: vec![],
            deferred_rx_lens: vec![],
            recv_returns: 0,
        };

        // Default trait implementation returns Vec::new().
        let drained = device.drain_deferred_rx();
        assert!(drained.is_empty());

        // Second call is also empty and idempotent.
        let drained = device.drain_deferred_rx();
        assert!(drained.is_empty());
    }

    // ── Protocol executor combined drain integration ──────────────────

    /// Verifies that a single recv+drain cycle correctly aggregates counts
    /// from all three counting paths: recv() return value (IP RX),
    /// drain_deferred_tx() (ARP TX), and drain_deferred_rx() (ARP RX).
    #[test]
    fn protocol_executor_three_path_combined_drain() {
        let mut device = test_device_handle(Box::new(CountingMockDevice {
            name: "mock",
            send_returns: 0,
            deferred_tx_lens: vec![60, 60], // 2 ARP TX frames (42+padding)
            deferred_rx_lens: vec![42],     // 1 ARP RX frame
            recv_returns: 1514,             // 1 IP RX frame
        }));

        // Simulate one protocol-executor RX drain iteration:
        //   1. recv IP frame → count_rx(frame_len)
        //   2. drain deferred TX → count_tx(each)
        //   3. drain deferred RX → count_rx(each)
        let frame_len = device.inner.recv(
            IF0,
            &mut test_packet_buffer(),
            Instant::from_millis(0),
            &mut |_| {},
        );
        if frame_len > 0 {
            device.count_rx(frame_len);
        }
        for len in device.inner.drain_deferred_tx() {
            device.count_tx(len);
        }
        for len in device.inner.drain_deferred_rx() {
            device.count_rx(len);
        }

        let snap = device.stats();
        // RX: 1 IP frame (1514) + 1 ARP frame (42) = 2 packets, 1556 bytes
        assert_eq!(snap.rx_packets, 2);
        assert_eq!(snap.rx_bytes, 1556);
        // TX: 2 ARP frames (60 + 60) = 2 packets, 120 bytes
        assert_eq!(snap.tx_packets, 2);
        assert_eq!(snap.tx_bytes, 120);
    }
}
