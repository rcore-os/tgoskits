//! Multi-device router used as the single smoltcp device.
//!
//! ax-net exposes one smoltcp `Interface` and one global `SocketSet`, then
//! places this router underneath as a virtual device that aggregates all
//! physical and virtual links. From smoltcp's perspective this module is a
//! single `Device`; internally it performs route lookup, source-address
//! selection, loopback delivery, and handoff to per-device workers.
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
//! - RX workers poll real devices and enqueue `RxPacket`s into a bounded shared
//!   RX queue. `Router::poll()` drains that queue into the smoltcp-facing packet
//!   buffer.
//! - smoltcp TX writes into `tx_buffer`. `Router::dispatch()` parses the IP
//!   destination, selects a route, and enqueues the packet to the chosen device
//!   worker.
//! - Loopback bypasses workers and the shared RX queue: dispatch copies directly
//!   from TX buffer to RX buffer and asks the protocol core to poll again.
//!
//! # Concurrency Rules
//!
//! Device workers only touch their `DeviceHandle` queues and concrete device
//! locks. Route lookup uses the shared route table read lock. Socket and service
//! locks are owned by the poll path, so worker threads must not call back into
//! socket operations.

use alloc::{
    boxed::Box,
    collections::VecDeque,
    format,
    string::{String, ToString},
    sync::{Arc, Weak},
    task::Wake,
    vec,
    vec::Vec,
};
use core::{
    sync::atomic::{AtomicUsize, Ordering},
    task::Waker,
};

use ax_hal::time::{NANOS_PER_MICROS, wall_time_nanos};
use ax_sync::Mutex;
use ax_task::WaitQueue;
use axpoll::IoEvents;
use smoltcp::{
    iface::SocketSet,
    phy::{DeviceCapabilities, Medium},
    storage::PacketMetadata,
    time::Instant,
    wire::{
        IpAddress, IpCidr, IpProtocol, IpVersion, Ipv4Address, Ipv4Cidr, Ipv4Packet, Ipv6Packet,
        TcpPacket,
    },
};
use spin::RwLock;

use crate::{
    LISTEN_TABLE,
    config::{DeviceBinding, InterfaceId, RouteInfo},
    consts::{DEVICE_RX_QUEUE_SIZE, DEVICE_TX_QUEUE_SIZE, SOCKET_BUFFER_SIZE, STANDARD_MTU},
    device::{ArpEntry, Device},
};

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

type PacketBuffer = smoltcp::storage::PacketBuffer<'static, InterfaceId>;
// TX metadata is created before route lookup; dispatch() selects the real
// egress interface from the packet destination and route table.
const TX_INTERFACE_PLACEHOLDER: InterfaceId = InterfaceId::new(0);

/// Bounded FIFO used between the protocol core and per-device workers.
struct BoundedPacketQueue<T> {
    inner: Mutex<VecDeque<T>>,
    capacity: usize,
    len: AtomicUsize,
}

impl<T> BoundedPacketQueue<T> {
    fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(VecDeque::with_capacity(capacity)),
            capacity,
            len: AtomicUsize::new(0),
        }
    }

    fn push(&self, packet: T) -> Result<(), T> {
        let mut inner = self.inner.lock();
        if inner.len() >= self.capacity {
            return Err(packet);
        }
        inner.push_back(packet);
        self.len.store(inner.len(), Ordering::Release);
        Ok(())
    }

    fn pop(&self) -> Option<T> {
        let mut inner = self.inner.lock();
        let packet = inner.pop_front();
        self.len.store(inner.len(), Ordering::Release);
        packet
    }

    fn is_empty(&self) -> bool {
        self.len.load(Ordering::Acquire) == 0
    }
}

struct TxPacket {
    /// Next-hop IP selected by the route table.
    next_hop: IpAddress,
    /// Complete IP packet to transmit.
    bytes: QueuedPacket,
}

struct RxPacket {
    /// Interface that received the packet.
    interface_id: InterfaceId,
    /// Complete IP packet received from a device.
    bytes: QueuedPacket,
}

/// Fixed-size packet storage for bounded router queues.
///
/// Keeping packets inline avoids per-packet heap allocation while preserving a
/// predictable memory ceiling from the queue capacity constants.
struct QueuedPacket {
    bytes: [u8; STANDARD_MTU],
    len: usize,
}

impl QueuedPacket {
    fn new(packet: &[u8]) -> Option<Self> {
        if packet.len() > STANDARD_MTU {
            return None;
        }
        let mut bytes = [0; STANDARD_MTU];
        bytes[..packet.len()].copy_from_slice(packet);
        Some(Self {
            bytes,
            len: packet.len(),
        })
    }

    fn as_slice(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

struct RouterQueues {
    /// Shared RX queue filled by device workers and drained by `Router::poll`.
    rx: Arc<BoundedPacketQueue<RxPacket>>,
}

/// Runtime handle for one physical or virtual device.
struct DeviceHandle {
    /// Stable interface id exposed to the control plane.
    interface_id: InterfaceId,
    /// Device name used for logs and userspace queries.
    name: String,
    /// Concrete device implementation.
    inner: Arc<Mutex<Box<dyn Device>>>,
    /// Shared router RX queue.
    rx_queue: Arc<BoundedPacketQueue<RxPacket>>,
    /// Per-device TX queue.
    tx_queue: Arc<BoundedPacketQueue<TxPacket>>,
    /// Wait queue used by the RX worker.
    rx_wake: Arc<WaitQueue>,
    /// Wait queue used by the TX worker.
    tx_wake: Arc<WaitQueue>,
    /// Waker registered into the concrete device.
    rx_waker: Waker,
}

impl DeviceHandle {
    fn new(
        interface_id: InterfaceId,
        device: Box<dyn Device>,
        queues: &Arc<RouterQueues>,
    ) -> Arc<Self> {
        let name = device.name().to_string();
        Arc::new_cyclic(|weak| Self {
            interface_id,
            name,
            inner: Arc::new(Mutex::new(device)),
            rx_queue: queues.rx.clone(),
            tx_queue: Arc::new(BoundedPacketQueue::new(DEVICE_TX_QUEUE_SIZE)),
            rx_wake: Arc::new(WaitQueue::new()),
            tx_wake: Arc::new(WaitQueue::new()),
            rx_waker: Waker::from(Arc::new(DeviceRxWake {
                device: weak.clone(),
            })),
        })
    }

    fn enqueue_tx(&self, next_hop: IpAddress, packet: &[u8]) -> bool {
        let Some(bytes) = QueuedPacket::new(packet) else {
            warn!(
                "{}: packet to {} exceeds MTU ({} bytes), dropping",
                self.name,
                next_hop,
                packet.len()
            );
            return false;
        };
        let tx = TxPacket { next_hop, bytes };
        if self.tx_queue.push(tx).is_err() {
            warn!(
                "{}: TX queue is full, dropping packet to {}",
                self.name, next_hop
            );
            return false;
        }
        self.tx_wake.notify_one(true);
        true
    }
}

fn register_device_poll(device: &DeviceHandle, waker: &core::task::Waker) {
    let poll = { device.inner.lock().readiness_poll() };
    if let Some(poll) = poll {
        // Device poll set is cloned while holding the device lock; registration
        // runs after releasing it.
        unsafe { poll.register(waker, IoEvents::IN | IoEvents::OUT | IoEvents::ERR) };
    }
}

fn wake_device_poll(device: &DeviceHandle) {
    let poll = { device.inner.lock().readiness_poll() };
    if let Some(poll) = poll {
        // Device readiness has been published by the net poll task, and the
        // device lock has been released before running wakers.
        unsafe { poll.wake(IoEvents::IN | IoEvents::OUT | IoEvents::ERR) };
    }
}

struct DeviceRxWake {
    device: Weak<DeviceHandle>,
}

impl Wake for DeviceRxWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        if let Some(device) = self.device.upgrade() {
            device.rx_wake.notify_one(true);
        }
    }
}

fn now() -> Instant {
    Instant::from_micros_const((wall_time_nanos() / NANOS_PER_MICROS) as i64)
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
    rx_buffer: PacketBuffer,
    tx_buffer: PacketBuffer,
    queues: Arc<RouterQueues>,
    devices: Vec<Arc<DeviceHandle>>,
    table: SharedRouteTable,
}
impl Router {
    /// Creates the virtual multi-device endpoint used by smoltcp.
    pub fn new(table: SharedRouteTable) -> Self {
        let rx_buffer = PacketBuffer::new(
            vec![PacketMetadata::EMPTY; SOCKET_BUFFER_SIZE],
            vec![0u8; STANDARD_MTU * SOCKET_BUFFER_SIZE],
        );
        let tx_buffer = PacketBuffer::new(
            vec![PacketMetadata::EMPTY; SOCKET_BUFFER_SIZE],
            vec![0u8; STANDARD_MTU * SOCKET_BUFFER_SIZE],
        );
        let queues = Arc::new(RouterQueues {
            rx: Arc::new(BoundedPacketQueue::new(DEVICE_RX_QUEUE_SIZE)),
        });
        Self {
            rx_buffer,
            tx_buffer,
            queues,
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
        self.devices
            .push(DeviceHandle::new(interface_id, device, &self.queues));
        self.devices.len() - 1
    }

    /// Returns the public interface id for a router device index.
    pub fn interface_id_for_dev(&self, dev: usize) -> Option<InterfaceId> {
        self.devices.get(dev).map(|device| device.interface_id)
    }

    /// Returns names of all registered devices.
    pub fn device_names(&self) -> Vec<String> {
        self.devices
            .iter()
            .map(|device| device.name.clone())
            .collect()
    }

    /// Starts TX workers for all non-loopback devices.
    pub fn start_tx_workers(&self) {
        for dev in 0..self.devices.len() {
            self.start_device_tx_worker(dev);
        }
    }

    /// Starts RX workers for all non-loopback devices.
    pub fn start_rx_workers(&self) {
        for dev in 0..self.devices.len() {
            self.start_device_rx_worker(dev);
        }
    }

    /// Starts RX/TX workers for one dynamically registered device.
    pub fn start_device_workers(&self, dev: usize) {
        self.start_device_rx_worker(dev);
        self.start_device_tx_worker(dev);
    }

    fn start_device_tx_worker(&self, dev: usize) {
        let Some(device) = self.devices.get(dev) else {
            return;
        };
        // Skip loopback: it uses fast path (no worker needed)
        if device.interface_id == InterfaceId::LOOPBACK {
            return;
        }
        let device = device.clone();
        let name = format!("{}-tx", device.name);
        ax_task::spawn_with_name(move || device_tx_worker(device), name);
    }

    fn start_device_rx_worker(&self, dev: usize) {
        let Some(device) = self.devices.get(dev) else {
            return;
        };
        // Skip loopback: packets injected directly in dispatch
        if device.interface_id == InterfaceId::LOOPBACK {
            return;
        }
        let device = device.clone();
        let name = format!("{}-rx", device.name);
        ax_task::spawn_with_name(move || device_rx_worker(device), name);
    }

    /// Finds the index of a device by its interface name (e.g. `"wlan0"`).
    pub fn device_index(&self, name: &str) -> Option<usize> {
        self.devices.iter().position(|device| device.name == name)
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
        self.devices[dev].inner.lock().set_ipv4_addr(address);

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
        // Drain worker-produced packets into the smoltcp-facing RX buffer.
        // smoltcp later consumes this buffer through Device::receive().
        let mut moved_rx = false;
        while !self.rx_buffer.is_full() {
            let Some(packet) = self.queues.rx.pop() else {
                break;
            };
            let bytes = packet.bytes.as_slice();
            snoop_tcp_packet(bytes, sockets);
            snoop(packet.interface_id, bytes);
            let Ok(dst) = self.rx_buffer.enqueue(bytes.len(), packet.interface_id) else {
                warn!("Router RX buffer is full, dropping packet");
                break;
            };
            dst.copy_from_slice(bytes);
            moved_rx = true;
        }
        moved_rx || !self.queues.rx.is_empty()
    }

    /// Sends a control-plane packet on a specific device.
    pub fn send_on_device(
        &mut self,
        dev: usize,
        next_hop: IpAddress,
        packet: &[u8],
        _timestamp: Instant,
    ) -> bool {
        let device = &self.devices[dev];
        if device.interface_id == InterfaceId::LOOPBACK {
            return inject_loopback_rx(&self.queues.rx, next_hop, packet);
        }
        device.enqueue_tx(next_hop, packet)
    }

    /// Collects ARP/neighbor entries from all devices.
    pub fn arp_entries(&self, timestamp: Instant) -> Vec<ArpEntry> {
        let mut entries = Vec::new();
        for device in &self.devices {
            entries.extend(device.inner.lock().arp_entries(timestamp));
        }
        entries
    }

    /// Registers a global device-readiness waker for all devices.
    pub fn register_device_waker(&self, waker: &core::task::Waker) {
        for device in &self.devices {
            register_device_poll(device, &device.rx_waker);
            register_device_poll(device, waker);
        }
    }

    /// Forces all device RX workers to re-check their devices.
    pub fn wake_all_devices(&self) {
        for device in &self.devices {
            wake_device_poll(device);
            device.rx_wake.notify_one(true);
        }
    }

    /// Registers a waker for devices allowed by a socket's binding.
    pub fn register_waker(&self, binding: DeviceBinding, waker: &core::task::Waker) {
        for device in &self.devices {
            if binding.bound_if.is_none_or(|id| id == device.interface_id) {
                register_device_poll(device, &device.rx_waker);
                register_device_poll(device, waker);
            }
        }
    }

    /// Routes smoltcp-emitted TX packets to loopback or device workers.
    pub fn dispatch(&mut self, _timestamp: Instant, sockets: &mut SocketSet<'_>) -> bool {
        let mut poll_next = false;
        while let Ok((_, packet)) = self.tx_buffer.dequeue() {
            match IpVersion::of_packet(packet).expect("got invalid IP packet") {
                IpVersion::Ipv4 => {
                    let packet = smoltcp::wire::Ipv4Packet::new_checked(packet)
                        .expect("got invalid IPv4 packet");
                    let src_addr = IpAddress::Ipv4(packet.src_addr());
                    let dst_addr = IpAddress::Ipv4(packet.dst_addr());
                    if packet.dst_addr().is_broadcast() {
                        let buf = packet.into_inner();
                        // Broadcast only to Ethernet devices (not loopback)
                        for dev in &self.devices {
                            if dev.interface_id != InterfaceId::LOOPBACK {
                                poll_next |= dev.enqueue_tx(dst_addr, buf);
                            }
                        }
                    } else {
                        let routes = self.table.read();
                        let Some(route) = routes.select_route_for_source(&dst_addr, &src_addr)
                        else {
                            warn!(
                                "No route found for source {} destination {}",
                                src_addr, dst_addr
                            );
                            continue;
                        };

                        let dev = &self.devices[route.dev];
                        if dev.interface_id == InterfaceId::LOOPBACK {
                            // Loopback packets are copied directly from the TX
                            // buffer into the RX buffer. This avoids the
                            // per-device worker and shared RX queue used by
                            // real devices.
                            poll_next |= inject_loopback_rx_direct(
                                &mut self.rx_buffer,
                                dst_addr,
                                packet.into_inner(),
                                sockets,
                            );
                        } else {
                            poll_next |= dev.enqueue_tx(route.next_hop, packet.into_inner());
                        }
                    }
                }
                IpVersion::Ipv6 => {
                    let packet = smoltcp::wire::Ipv6Packet::new_checked(packet)
                        .expect("got invalid IPv6 packet");
                    let src_addr = IpAddress::Ipv6(packet.src_addr());
                    let dst_addr = IpAddress::Ipv6(packet.dst_addr());
                    if packet.dst_addr().is_multicast() {
                        let buf = packet.into_inner();
                        // Multicast only to Ethernet devices (not loopback)
                        for dev in &self.devices {
                            if dev.interface_id != InterfaceId::LOOPBACK {
                                poll_next |= dev.enqueue_tx(dst_addr, buf);
                            }
                        }
                    } else {
                        let routes = self.table.read();
                        let Some(route) = routes.select_route_for_source(&dst_addr, &src_addr)
                        else {
                            warn!(
                                "No route found for source {} destination {}",
                                src_addr, dst_addr
                            );
                            continue;
                        };

                        let dev = &self.devices[route.dev];
                        if dev.interface_id == InterfaceId::LOOPBACK {
                            poll_next |= inject_loopback_rx_direct(
                                &mut self.rx_buffer,
                                dst_addr,
                                packet.into_inner(),
                                sockets,
                            );
                        } else {
                            poll_next |= dev.enqueue_tx(route.next_hop, packet.into_inner());
                        }
                    }
                }
            }
        }
        poll_next
    }
}

/// Injects a loopback packet directly into the smoltcp-facing RX buffer.
fn inject_loopback_rx_direct(
    rx_buffer: &mut PacketBuffer,
    dst_addr: IpAddress,
    packet: &[u8],
    sockets: &mut SocketSet<'_>,
) -> bool {
    snoop_tcp_packet(packet, sockets);
    let Ok(dst) = rx_buffer.enqueue(packet.len(), InterfaceId::LOOPBACK) else {
        warn!("Loopback: RX buffer full, dropping packet to {}", dst_addr);
        return false;
    };
    dst.copy_from_slice(packet);
    true
}

/// Injects a loopback packet into the router RX queue.
///
/// Returns `true` when the packet was queued; callers should continue polling
/// so smoltcp can immediately consume the injected RX packet.
fn inject_loopback_rx(
    rx_queue: &BoundedPacketQueue<RxPacket>,
    dst_addr: IpAddress,
    packet: &[u8],
) -> bool {
    let Some(bytes) = QueuedPacket::new(packet) else {
        warn!(
            "Loopback: packet to {} exceeds MTU ({} bytes), dropping",
            dst_addr,
            packet.len()
        );
        return false;
    };
    let rx = RxPacket {
        interface_id: InterfaceId::LOOPBACK,
        bytes,
    };
    if rx_queue.push(rx).is_err() {
        warn!("Loopback: RX queue full, dropping packet to {}", dst_addr);
        return false;
    }
    true
}

/// Dedicated worker that drains one device's TX queue.
fn device_tx_worker(device: Arc<DeviceHandle>) {
    loop {
        if let Some(packet) = device.tx_queue.pop() {
            let poll_next =
                device
                    .inner
                    .lock()
                    .send(packet.next_hop, packet.bytes.as_slice(), now());
            if poll_next {
                crate::request_poll();
            }
        } else {
            device.tx_wake.wait_until(|| !device.tx_queue.is_empty());
        }
    }
}

/// Dedicated worker that polls one device and forwards packets to router RX.
fn device_rx_worker(device: Arc<DeviceHandle>) {
    let mut rx_buffer = PacketBuffer::new(vec![PacketMetadata::EMPTY; 1], vec![0u8; STANDARD_MTU]);

    loop {
        let mut received = false;
        {
            let mut device_inner = device.inner.lock();
            let mut snoop = |_packet: &[u8]| {};
            while rx_buffer.is_empty()
                && device_inner.recv(device.interface_id, &mut rx_buffer, now(), &mut snoop)
            {
                received = true;
            }
        }

        while let Ok((interface_id, packet)) = rx_buffer.dequeue() {
            let rx = RxPacket {
                interface_id,
                bytes: match QueuedPacket::new(packet) {
                    Some(bytes) => bytes,
                    None => {
                        warn!(
                            "{}: RX packet exceeds MTU ({} bytes), dropping",
                            device.name,
                            packet.len()
                        );
                        continue;
                    }
                },
            };
            if device.rx_queue.push(rx).is_err() {
                warn!("{}: RX queue is full, dropping packet", device.name);
                crate::request_poll();
                ax_task::yield_now();
                break;
            }
            crate::request_poll();
            received = true;
        }

        if !received {
            register_device_poll(&device, &device.rx_waker);
            device.rx_wake.wait();
        }
    }
}

/// smoltcp TX token backed by the router's temporary TX buffer.
pub struct TxToken<'a>(&'a mut PacketBuffer);

impl smoltcp::phy::TxToken for TxToken<'_> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        // TX metadata is ignored: Router::dispatch parses the emitted IP
        // packet and selects the actual egress interface from the route table.
        f(self
            .0
            .enqueue(len, TX_INTERFACE_PLACEHOLDER)
            .expect("This was checked before creating the TxToken"))
    }
}

/// Detects passive TCP opens before smoltcp consumes the incoming packet.
fn snoop_tcp_packet(buf: &[u8], sockets: &mut SocketSet<'_>) {
    let (protocol, src_addr, dst_addr, payload) = match IpVersion::of_packet(buf).unwrap() {
        IpVersion::Ipv4 => {
            let packet = Ipv4Packet::new_unchecked(buf);
            (
                packet.next_header(),
                IpAddress::Ipv4(packet.src_addr()),
                IpAddress::Ipv4(packet.dst_addr()),
                packet.payload(),
            )
        }
        IpVersion::Ipv6 => {
            let packet = Ipv6Packet::new_unchecked(buf);
            (
                packet.next_header(),
                IpAddress::Ipv6(packet.src_addr()),
                IpAddress::Ipv6(packet.dst_addr()),
                packet.payload(),
            )
        }
    };
    if protocol == IpProtocol::Tcp {
        let tcp_packet = TcpPacket::new_unchecked(payload);
        let src_addr = (src_addr, tcp_packet.src_port()).into();
        let dst_addr = (dst_addr, tcp_packet.dst_port()).into();
        let is_first = tcp_packet.syn() && !tcp_packet.ack();
        if is_first {
            LISTEN_TABLE.incoming_tcp_packet(src_addr, dst_addr, sockets);
        }
    }
}

/// smoltcp RX token for one packet queued by the router.
pub struct RxToken<'a> {
    interface_id: InterfaceId,
    packet: &'a [u8],
}

impl<'a> smoltcp::phy::RxToken for RxToken<'a> {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        let _ingress_if = self.interface_id;
        f(self.packet)
    }
}

impl smoltcp::phy::Device for Router {
    type RxToken<'a> = RxToken<'a>;
    type TxToken<'a> = TxToken<'a>;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        if self.rx_buffer.is_empty() || self.tx_buffer.is_full() {
            None
        } else {
            Some((
                {
                    let (interface_id, packet) = self.rx_buffer.dequeue().unwrap();
                    RxToken {
                        interface_id,
                        packet,
                    }
                },
                TxToken(&mut self.tx_buffer),
            ))
        }
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
        caps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IF0: InterfaceId = InterfaceId::new(2);
    const IF1: InterfaceId = InterfaceId::new(3);
    const SRC0: IpAddress = IpAddress::Ipv4(Ipv4Address::new(10, 0, 0, 2));
    const SRC1: IpAddress = IpAddress::Ipv4(Ipv4Address::new(10, 0, 1, 2));

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

    #[test]
    fn bounded_packet_queue_reports_full_and_preserves_order() {
        let queue = BoundedPacketQueue::new(2);
        assert!(queue.is_empty());
        assert!(queue.push(1).is_ok());
        assert!(queue.push(2).is_ok());
        assert!(queue.push(3).is_err());
        assert!(!queue.is_empty());
        assert_eq!(queue.pop(), Some(1));
        assert_eq!(queue.pop(), Some(2));
        assert_eq!(queue.pop(), None);
        assert!(queue.is_empty());
    }
}
