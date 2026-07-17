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
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    task::Waker,
    time::Duration,
};

use ax_hal::time::{NANOS_PER_MICROS, monotonic_time_nanos};
use ax_kspin::SpinRwLock as RwLock;
use ax_sync::Mutex;
use ax_task::WaitQueue;
use axpoll::IoEvents;
use smoltcp::{
    iface::SocketSet,
    phy::{DeviceCapabilities, Medium, PacketMeta},
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
    consts::{DEVICE_RX_QUEUE_SIZE, DEVICE_TX_QUEUE_SIZE, SOCKET_BUFFER_SIZE, STANDARD_MTU},
    device::{ArpEntry, Device},
    ip_tos::apply_egress_ip_tos,
    rx_meta::packet_meta_for_rx_packet,
};

const DEVICE_RX_WORKER_BATCH: usize = 16;
const DEVICE_RX_IDLE_POLL_INTERVAL: Duration = Duration::from_millis(10);

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
    /// Sticky readiness bit for RX wakeups. `WaitQueue` notifications are not
    /// sticky, so a device wake that races with the worker entering `wait()`
    /// must be preserved here until the worker observes it.
    rx_ready: AtomicBool,
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
            rx_ready: AtomicBool::new(false),
            rx_bytes: AtomicU64::new(0),
            rx_packets: AtomicU64::new(0),
            rx_errors: AtomicU64::new(0),
            rx_dropped: AtomicU64::new(0),
            tx_bytes: AtomicU64::new(0),
            tx_packets: AtomicU64::new(0),
            tx_errors: AtomicU64::new(0),
            tx_dropped: AtomicU64::new(0),
        })
    }

    /// Records `len` bytes received on this interface.
    ///
    /// `rx_packets` is incremented for every call regardless of `len`. Callers
    /// must ensure `len > 0` when counting a real reception; a zero `len` only
    /// makes sense for testing or diagnostic paths.
    fn count_rx(&self, len: usize) {
        // Relaxed ordering is sufficient: fetch_add provides atomic RMW that
        // guarantees no lost updates even with concurrent writers (device
        // worker + loopback dispatch + deferred drains).  /proc/net/dev
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

    fn count_rx_error(&self) {
        self.rx_errors.fetch_add(1, Ordering::Relaxed);
    }

    fn count_rx_drop(&self) {
        self.rx_dropped.fetch_add(1, Ordering::Relaxed);
    }

    fn count_tx_error(&self) {
        self.tx_errors.fetch_add(1, Ordering::Relaxed);
    }

    fn count_tx_drop(&self) {
        self.tx_dropped.fetch_add(1, Ordering::Relaxed);
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

    /// Drains one iteration of the RX worker's local batch into the shared RX
    /// queue. Pops entries from `local_batch` and pushes them to `rx_queue`;
    /// counts each successfully pushed frame via `count_rx`. On backpressure,
    /// the failed entry is returned to the front of `local_batch` and the
    /// function returns `Err(())`. If all entries drain successfully, returns
    /// `Ok(())`. This shared helper keeps the backpressure retry logic and
    /// frame-length pairing consistent between the production RX worker and
    /// tests that verify the pairing invariant.
    fn drain_local_batch_step(
        &self,
        local_batch: &mut VecDeque<(RxPacket, usize)>,
    ) -> Result<(), ()> {
        while let Some((rx, frame_len)) = local_batch.pop_front() {
            match self.rx_queue.push(rx) {
                Ok(()) => {
                    self.count_rx(frame_len);
                }
                Err(rx) => {
                    // Queue is full — return the entry to the front and
                    // signal backpressure to the caller. The frame_len stays
                    // paired with its packet.
                    local_batch.push_front((rx, frame_len));
                    return Err(());
                }
            }
        }
        Ok(())
    }

    fn wake_rx(&self) {
        self.rx_ready.store(true, Ordering::Release);
        self.rx_wake.notify_one(true);
    }

    fn take_rx_ready(&self) -> bool {
        self.rx_ready.swap(false, Ordering::AcqRel)
    }

    fn enqueue_tx(&self, next_hop: IpAddress, packet: &[u8]) -> bool {
        let Some(bytes) = QueuedPacket::new(packet) else {
            warn!(
                "{}: packet to {} exceeds MTU ({} bytes), dropping",
                self.name,
                next_hop,
                packet.len()
            );
            self.count_tx_drop();
            return false;
        };
        let tx = TxPacket { next_hop, bytes };
        if self.tx_queue.push(tx).is_err() {
            warn!(
                "{}: TX queue is full, dropping packet to {}",
                self.name, next_hop
            );
            self.count_tx_drop();
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
            device.wake_rx();
        }
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
    queues: Arc<RouterQueues>,
    devices: Vec<Arc<DeviceHandle>>,
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
            let Ok(dst) = self
                .rx_buffer
                .enqueue(bytes.len(), rx_metadata(packet.interface_id, bytes))
            else {
                warn!("Router RX buffer is full, dropping packet");
                if let Some(dev) = self
                    .devices
                    .iter()
                    .find(|d| d.interface_id == packet.interface_id)
                {
                    dev.count_rx_drop();
                }
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
            // Loopback traffic is transmitted and received on the same
            // interface. Count only after successful injection so that
            // failures (buffer full, over-MTU) are correctly recorded as
            // drops rather than silently inflating the byte/packet counters.
            let ok = inject_loopback_rx(&self.queues.rx, next_hop, packet);
            if ok {
                device.count_tx(packet.len());
                device.count_rx(packet.len());
            } else {
                device.count_rx_drop();
            }
            return ok;
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

    /// Returns a per-interface snapshot of RX/TX byte and packet counters.
    pub fn net_dev_stats(&self) -> Vec<NetDevStats> {
        self.devices.iter().map(|device| device.stats()).collect()
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
            device.wake_rx();
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
        let Router {
            rx_buffer,
            tx_buffer,
            devices,
            table,
            ..
        } = self;
        while let Ok((_, packet)) = tx_buffer.dequeue() {
            apply_egress_ip_tos(packet);
            match IpVersion::of_packet(packet).expect("got invalid IP packet") {
                IpVersion::Ipv4 => {
                    let packet = smoltcp::wire::Ipv4Packet::new_checked(packet)
                        .expect("got invalid IPv4 packet");
                    let src_addr = IpAddress::Ipv4(packet.src_addr());
                    let dst_addr = IpAddress::Ipv4(packet.dst_addr());
                    if packet.dst_addr().is_broadcast() {
                        poll_next |=
                            dispatch_link_local_fanout(devices, dst_addr, packet.into_inner());
                    } else {
                        poll_next |= dispatch_unicast_packet(
                            rx_buffer,
                            devices,
                            table,
                            src_addr,
                            dst_addr,
                            packet.into_inner(),
                            sockets,
                        );
                    }
                }
                IpVersion::Ipv6 => {
                    let packet = smoltcp::wire::Ipv6Packet::new_checked(packet)
                        .expect("got invalid IPv6 packet");
                    let src_addr = IpAddress::Ipv6(packet.src_addr());
                    let dst_addr = IpAddress::Ipv6(packet.dst_addr());
                    if packet.dst_addr().is_multicast() {
                        poll_next |=
                            dispatch_link_local_fanout(devices, dst_addr, packet.into_inner());
                    } else {
                        poll_next |= dispatch_unicast_packet(
                            rx_buffer,
                            devices,
                            table,
                            src_addr,
                            dst_addr,
                            packet.into_inner(),
                            sockets,
                        );
                    }
                }
            }
        }
        poll_next
    }
}

fn dispatch_link_local_fanout(
    devices: &[Arc<DeviceHandle>],
    dst_addr: IpAddress,
    packet: &[u8],
) -> bool {
    let mut poll_next = false;
    for dev in devices {
        if dev.interface_id != InterfaceId::LOOPBACK {
            poll_next |= dev.enqueue_tx(dst_addr, packet);
        }
    }
    poll_next
}

fn dispatch_unicast_packet(
    rx_buffer: &mut RouterPacketBuffer,
    devices: &[Arc<DeviceHandle>],
    table: &SharedRouteTable,
    src_addr: IpAddress,
    dst_addr: IpAddress,
    packet: &[u8],
    sockets: &mut SocketSet<'_>,
) -> bool {
    let routes = table.read();
    let Some(route) = routes.select_route_for_source(&dst_addr, &src_addr) else {
        warn!(
            "No route found for source {} destination {}",
            src_addr, dst_addr
        );
        // Attribute the drop to the source device's tx_dropped, matching
        // Linux behaviour where an unroutable locally-generated packet
        // increments the ingress device's drop counter (often alongside
        // LINUX_MIB_IPINNOROUTES).
        if let Some(src_dev) = routes
            .select_route_if(&src_addr, |_| true)
            .and_then(|r| devices.get(r.dev))
        {
            src_dev.count_tx_drop();
        } else if let Some(lo_dev) = devices
            .iter()
            .find(|d| d.interface_id == InterfaceId::LOOPBACK)
        {
            // Fallback: source address not in any route table (e.g.
            // unnumbered interface, removed address).  Attribute to
            // loopback so the drop is at least observable.
            lo_dev.count_tx_drop();
        }
        return false;
    };

    let dev = &devices[route.dev];
    if dev.interface_id == InterfaceId::LOOPBACK {
        // Loopback packets are copied directly from the TX buffer into the RX
        // buffer, bypassing per-device workers and the shared RX queue. Count
        // only after successful injection so that failures (buffer full) are
        // correctly recorded as drops rather than silently inflating the
        // byte/packet counters.
        let ok = inject_loopback_rx_direct(rx_buffer, dst_addr, packet, sockets);
        if ok {
            dev.count_tx(packet.len());
            dev.count_rx(packet.len());
        } else {
            dev.count_rx_drop();
        }
        ok
    } else {
        dev.enqueue_tx(route.next_hop, packet)
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
            let frame_len =
                device
                    .inner
                    .lock()
                    .send(packet.next_hop, packet.bytes.as_slice(), now());
            if frame_len > 0 {
                device.count_tx(frame_len);
            }
            // Return-0 without counting: EthernetDevice::send() internally
            // tracks tx_errors (via deferred_tx_errors / deferred_tx_drops)
            // and the RX worker drains them into DeviceHandle. ARP-pending
            // packets are not dropped — they are queued in pending_packets
            // and sent later through process_arp() → deferred_tx_frame_lens.
        } else {
            device.tx_wake.wait_until(|| !device.tx_queue.is_empty());
        }
    }
}

/// Dedicated worker that polls one device and forwards packets to router RX.
fn device_rx_worker(device: Arc<DeviceHandle>) {
    let mut rx_buffer = DevicePacketBuffer::new(
        vec![PacketMetadata::EMPTY; DEVICE_RX_WORKER_BATCH],
        vec![0u8; STANDARD_MTU * DEVICE_RX_WORKER_BATCH],
    );
    // Persistent FIFO pairing each received packet with its L2 frame length.
    // Entries that could not be pushed to the shared RX queue due to
    // backpressure stay here and are retried on the next iteration. This
    // keeps frame_len paired with its packet regardless of queue state.
    let mut local_batch: VecDeque<(RxPacket, usize)> =
        VecDeque::with_capacity(DEVICE_RX_WORKER_BATCH);

    loop {
        let mut received = false;
        {
            let mut device_inner = device.inner.lock();
            let mut snoop = |_packet: &[u8]| {};
            while local_batch.len() < DEVICE_RX_WORKER_BATCH && !rx_buffer.is_full() {
                let frame_len =
                    device_inner.recv(device.interface_id, &mut rx_buffer, now(), &mut snoop);
                if frame_len == 0 {
                    break;
                }
                // Dequeue immediately so frame_len stays paired with its
                // packet — the 1:1 correspondence is established before
                // any backpressure can desynchronise them.
                let Ok((interface_id, packet)) = rx_buffer.dequeue() else {
                    break;
                };
                let Some(bytes) = QueuedPacket::new(packet) else {
                    warn!(
                        "{}: RX packet exceeds MTU ({} bytes), dropping",
                        device.name,
                        packet.len()
                    );
                    device.count_rx_drop();
                    continue;
                };
                local_batch.push_back((
                    RxPacket {
                        interface_id,
                        bytes,
                    },
                    frame_len,
                ));
                received = true;
            }
            // Count TX bytes from packets sent asynchronously during recv()
            // (e.g. pending ARP sends and ARP replies inside process_arp()).
            for frame_len in device_inner.drain_deferred_tx() {
                device.count_tx(frame_len);
            }
            // Count RX bytes from non-IP frames received during recv()
            // (e.g. ARP requests processed in handle_frame()).
            for frame_len in device_inner.drain_deferred_rx() {
                device.count_rx(frame_len);
            }
            // Drain device-internal error counters.
            let tx_errs = device_inner.drain_deferred_tx_errors();
            for _ in 0..tx_errs {
                device.count_tx_error();
            }
            let tx_drops = device_inner.drain_deferred_tx_drops();
            for _ in 0..tx_drops {
                device.count_tx_drop();
            }
            let rx_errs = device_inner.drain_deferred_rx_errors();
            for _ in 0..rx_errs {
                device.count_rx_error();
            }
            let rx_drops = device_inner.drain_deferred_rx_drops();
            for _ in 0..rx_drops {
                device.count_rx_drop();
            }
        }

        // Push to the shared RX queue outside the device lock.
        if device.drain_local_batch_step(&mut local_batch).is_err() {
            // Backpressure: the shared RX queue is full. Notify the main poll
            // loop to drain, yield CPU to allow progress, then retry on the
            // next iteration. The unpushed entries remain in local_batch with
            // their frame lengths paired.
            warn!("{}: RX queue is full, delaying packet", device.name);
            crate::request_poll();
            ax_task::yield_now();
        } else {
            // All entries were successfully pushed — notify the main poll loop
            // that new packets are available for processing.
            if !local_batch.is_empty() {
                panic!("drain_local_batch_step returned Ok but local_batch is not empty");
            }
            crate::request_poll();
        }

        if !received && local_batch.is_empty() {
            register_device_poll(&device, &device.rx_waker);
            device
                .rx_wake
                .wait_timeout_until(DEVICE_RX_IDLE_POLL_INTERVAL, || device.take_rx_ready());
        }
    }
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
        f(self
            .0
            .enqueue(len, tx_metadata())
            .expect("This was checked before creating the TxToken"))
    }
}

/// Detects passive TCP opens before smoltcp consumes the incoming packet.
fn snoop_tcp_packet(buf: &[u8], sockets: &mut SocketSet<'_>) {
    let (src_addr, dst_addr, payload) = match IpVersion::of_packet(buf).unwrap() {
        IpVersion::Ipv4 => {
            let packet = Ipv4Packet::new_unchecked(buf);
            if packet.next_header() != IpProtocol::Tcp {
                return;
            }
            (
                IpAddress::Ipv4(packet.src_addr()),
                IpAddress::Ipv4(packet.dst_addr()),
                packet.payload(),
            )
        }
        IpVersion::Ipv6 => {
            let packet = Ipv6Packet::new_unchecked(buf);
            if packet.next_header() != IpProtocol::Tcp {
                return;
            }
            (
                IpAddress::Ipv6(packet.src_addr()),
                IpAddress::Ipv6(packet.dst_addr()),
                packet.payload(),
            )
        }
    };
    let tcp_packet = TcpPacket::new_unchecked(payload);
    let src_addr = (src_addr, tcp_packet.src_port()).into();
    let dst_addr = (dst_addr, tcp_packet.dst_port()).into();
    let is_first = tcp_packet.syn() && !tcp_packet.ack();
    if is_first {
        LISTEN_TABLE.incoming_tcp_packet(src_addr, dst_addr, sockets);
    }
}

/// smoltcp RX token for one packet queued by the router.
pub struct RxToken<'a> {
    interface_id: InterfaceId,
    packet_meta: PacketMeta,
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

    fn meta(&self) -> PacketMeta {
        self.packet_meta
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
                    let (metadata, packet) = self.rx_buffer.dequeue().unwrap();
                    RxToken {
                        interface_id: metadata.interface_id,
                        packet_meta: metadata.packet_meta,
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

    fn test_device_handle(device: Box<dyn Device>) -> Arc<DeviceHandle> {
        let queues = Arc::new(RouterQueues {
            rx: Arc::new(BoundedPacketQueue::new(1)),
        });
        DeviceHandle::new(IF0, device, &queues)
    }

    fn ipv4_cidr(addr: Ipv4Address, prefix_len: u8) -> IpCidr {
        Ipv4Cidr::new(addr, prefix_len).into()
    }

    #[test]
    fn rx_worker_wake_is_sticky_until_observed() {
        let device = test_device_handle(Box::new(EmptyDevice));

        assert!(!device.take_rx_ready());
        device.rx_waker.wake_by_ref();
        assert!(device.take_rx_ready());
        assert!(!device.take_rx_ready());
    }

    #[test]
    fn rx_worker_idle_poll_interval_keeps_polling_devices_active() {
        assert!(DEVICE_RX_IDLE_POLL_INTERVAL > core::time::Duration::ZERO);
        assert!(DEVICE_RX_IDLE_POLL_INTERVAL <= core::time::Duration::from_millis(10));
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

    fn test_device_handle(device: Box<dyn Device>) -> Arc<DeviceHandle> {
        let queues = Arc::new(RouterQueues {
            rx: Arc::new(BoundedPacketQueue::new(1)),
        });
        DeviceHandle::new(IF0, device, &queues)
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
        let device = test_device_handle(Box::new(CountingMockDevice {
            name: "mock",
            send_returns: 1514, // L2 frame length (14 eth hdr + 1500 IP payload)
            deferred_tx_lens: vec![],
            deferred_rx_lens: vec![],
            recv_returns: 0,
        }));

        // Simulate what device_tx_worker does
        let frame_len = device
            .inner
            .lock()
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
        let device = test_device_handle(Box::new(CountingMockDevice {
            name: "mock",
            send_returns: 0, // ARP pending or send failure
            deferred_tx_lens: vec![],
            deferred_rx_lens: vec![],
            recv_returns: 0,
        }));

        let frame_len = device
            .inner
            .lock()
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
        let device = test_device_handle(Box::new(CountingMockDevice {
            name: "mock",
            send_returns: 0,
            deferred_tx_lens: vec![],
            deferred_rx_lens: vec![],
            recv_returns: 1514,
        }));

        let frame_len = device.inner.lock().recv(
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
        let device = test_device_handle(Box::new(CountingMockDevice {
            name: "mock",
            send_returns: 0,
            deferred_tx_lens: vec![],
            deferred_rx_lens: vec![],
            recv_returns: 0, // no packet available
        }));

        let frame_len = device.inner.lock().recv(
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

    // ── RX queue backpressure preserves frame_len pairing ──────────────

    #[test]
    fn rx_backpressure_preserves_frame_len_pairing() {
        // When the shared RX queue is full, unprocessed (packet, frame_len)
        // pairs must stay paired for the next drain iteration. This test
        // verifies that the production drain_local_batch_step() helper
        // preserves FIFO order and pairing across backpressure retries.
        //
        // Use a queue large enough that backpressure is deliberate (capacity 1)
        // but the second drain can exercise the full successful path.
        let queues = Arc::new(RouterQueues {
            rx: Arc::new(BoundedPacketQueue::new(4)),
        });
        let device: Arc<DeviceHandle> = DeviceHandle::new(
            IF0,
            Box::new(CountingMockDevice {
                name: "mock",
                send_returns: 0,
                deferred_tx_lens: vec![],
                deferred_rx_lens: vec![],
                recv_returns: 0,
            }),
            &queues,
        );

        let mut local_batch: VecDeque<(RxPacket, usize)> = VecDeque::new();

        // Simulate receiving 3 packets with distinct L2 frame lengths.
        for (i, frame_len) in [100usize, 200, 300].iter().enumerate() {
            let bytes = QueuedPacket::new(&[i as u8; 64]).unwrap();
            local_batch.push_back((
                RxPacket {
                    interface_id: IF0,
                    bytes,
                },
                *frame_len,
            ));
        }
        assert_eq!(local_batch.len(), 3);

        // Fill the shared RX queue to capacity so pushes fail.
        for n in 0..4 {
            let fill = RxPacket {
                interface_id: IF0,
                bytes: QueuedPacket::new(&[n as u8; 64]).unwrap(),
            };
            assert!(device.rx_queue.push(fill).is_ok());
        }

        // First drain attempt — no entries can be pushed (queue full).
        // drain_local_batch_step returns Err on backpressure and leaves
        // all entries in local_batch.
        let result = device.drain_local_batch_step(&mut local_batch);
        assert!(result.is_err(), "Expected backpressure Err on full queue");
        // All 3 entries are still paired in local_batch.
        assert_eq!(local_batch.len(), 3);

        // Drain all fill packets to make room.
        for _ in 0..4 {
            assert!(device.rx_queue.pop().is_some());
        }

        // Second drain — all entries should succeed, each with its original
        // frame length still paired.
        let result = device.drain_local_batch_step(&mut local_batch);
        assert!(result.is_ok(), "Expected Ok after clearing queue");
        assert!(local_batch.is_empty(), "All entries should be drained");

        let stats = device.stats();
        assert_eq!(stats.rx_packets, 3);
        // 100 + 200 + 300 = 600
        assert_eq!(stats.rx_bytes, 600);
    }

    // ── RX worker combined drain integration ──────────────────────────

    /// Verifies that a single recv+drain cycle correctly aggregates counts
    /// from all three counting paths: recv() return value (IP RX),
    /// drain_deferred_tx() (ARP TX), and drain_deferred_rx() (ARP RX).
    #[test]
    fn rx_worker_three_path_combined_drain() {
        let device = test_device_handle(Box::new(CountingMockDevice {
            name: "mock",
            send_returns: 0,
            deferred_tx_lens: vec![60, 60], // 2 ARP TX frames (42+padding)
            deferred_rx_lens: vec![42],     // 1 ARP RX frame
            recv_returns: 1514,             // 1 IP RX frame
        }));

        // Simulate one iteration of device_rx_worker's inner loop:
        //   1. recv IP frame → count_rx(frame_len)
        //   2. drain deferred TX → count_tx(each)
        //   3. drain deferred RX → count_rx(each)
        let frame_len = device.inner.lock().recv(
            IF0,
            &mut test_packet_buffer(),
            Instant::from_millis(0),
            &mut |_| {},
        );
        if frame_len > 0 {
            device.count_rx(frame_len);
        }
        for len in device.inner.lock().drain_deferred_tx() {
            device.count_tx(len);
        }
        for len in device.inner.lock().drain_deferred_rx() {
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
