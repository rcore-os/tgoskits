//! Backends for the AxVisor virtio-net device model.
//!
//! Two variants share the [`AxvisorNetworkBackend`] enum:
//! - [`DeterministicUdpEchoBackend`] is the no-network regression oracle. It
//!   inspects each guest TX frame and enqueues a deterministic ARP/UDP reply on
//!   a bounded RX queue that the guest delivery worker drains. Nothing about it
//!   depends on a host NIC, the switch or a real network.
//! - [`RawUplinkBackend`] wraps a [`PortEndpoint`]: the per-guest-port data
//!   plane of the shared uplink switch. Guest TX (`transmit`) is pushed onto the
//!   port's egress queue and wakes the single uplink worker; the switch/uplink
//!   path pushes inbound frames onto the port's ingress queue, which the guest
//!   delivery worker drains.
//!
//! `transmit` runs inside the device's TX queue lock, so it only validates and
//! enqueues. It never re-enters the device and never injects an IRQ directly.

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use ax_kspin::SpinNoIrq as Mutex;
use axvirtio_net::{NetworkBackend, NetworkBackendError};
use axvirtio_switch::{SwitchPort, SwitchPortId};
use axvm::WorkerWaitQueue;

use super::config::{
    ECHO_UDP_PORT, GUEST_IPV4, PEER_IPV4, PEER_MAC, RX_QUEUE_CAPACITY, TX_QUEUE_CAPACITY,
};
use super::raw_uplink::UplinkWorkSignal;

#[derive(Clone)]
pub enum AxvisorNetworkBackend {
    Deterministic(DeterministicUdpEchoBackend),
    RawUplink(RawUplinkBackend),
}

impl AxvisorNetworkBackend {
    pub fn deterministic(guest_mac: [u8; 6]) -> Self {
        Self::Deterministic(DeterministicUdpEchoBackend::new(guest_mac))
    }

    pub fn raw_uplink(endpoint: Arc<PortEndpoint>) -> Self {
        Self::RawUplink(RawUplinkBackend { endpoint })
    }

    pub fn drain_rx(&self) -> Option<Vec<u8>> {
        match self {
            Self::Deterministic(backend) => backend.drain_rx(),
            Self::RawUplink(backend) => backend.endpoint.drain_ingress(),
        }
    }

    pub fn rx_ready(&self) -> bool {
        match self {
            Self::Deterministic(backend) => backend.rx_ready(),
            Self::RawUplink(backend) => backend.endpoint.ingress_ready(),
        }
    }

    pub fn clear_rx_ready(&self) {
        match self {
            Self::Deterministic(backend) => backend.clear_rx_ready(),
            Self::RawUplink(backend) => backend.endpoint.clear_ingress_ready(),
        }
    }

    pub fn requeue_rx(&self, frame: Vec<u8>) {
        match self {
            Self::Deterministic(backend) => backend.requeue_rx(frame),
            Self::RawUplink(backend) => backend.endpoint.requeue_ingress(frame),
        }
    }

    pub fn wake_queue(&self) -> &WorkerWaitQueue {
        match self {
            Self::Deterministic(backend) => backend.wake_queue(),
            Self::RawUplink(backend) => backend.endpoint.guest_wake(),
        }
    }

    pub fn wake_worker(&self) {
        match self {
            Self::Deterministic(backend) => backend.wake_worker(),
            Self::RawUplink(backend) => backend.endpoint.wake_guest(),
        }
    }
}

impl NetworkBackend for AxvisorNetworkBackend {
    fn transmit(&self, frame: &[u8]) -> Result<(), NetworkBackendError> {
        match self {
            Self::Deterministic(backend) => backend.transmit(frame),
            Self::RawUplink(backend) => backend.transmit(frame),
        }
    }

    fn rx_queue_notified(&self) {
        match self {
            Self::Deterministic(backend) => backend.rx_queue_notified(),
            Self::RawUplink(backend) => backend.endpoint.on_guest_rx_kick(),
        }
    }
}

/// Per-guest data plane for one switch-backed virtio-net port.
///
/// One endpoint exists per guest NIC. It is shared (via [`Arc`]) between:
/// - the device model — `transmit` pushes guest TX onto the **egress** queue;
/// - the switch — [`SwitchPort::deliver_ingress`] pushes switched/host frames
///   onto the **ingress** queue;
/// - the guest delivery worker — drains the **ingress** queue into the device;
/// - the uplink worker — round-robin pops the **egress** queue.
///
/// Egress and ingress have independent readiness so one global boolean cannot
/// conflate host IRQ readiness with this port's TX/RX (design §6.2).
pub struct PortEndpoint {
    id: SwitchPortId,
    guest_mac: [u8; 6],
    egress: Mutex<VecDeque<Vec<u8>>>,
    ingress: Mutex<VecDeque<Vec<u8>>>,
    uplink_signal: Arc<UplinkWorkSignal>,
    guest_wake: WorkerWaitQueue,
    ingress_ready: AtomicBool,
    active: AtomicBool,
    counters: PortCounters,
}

impl PortEndpoint {
    /// Creates an inactive endpoint bound to one uplink work signal. The
    /// factory activates it (`activate`) once it is registered with the switch
    /// and the guest delivery worker is about to start.
    pub(super) fn new(
        id: SwitchPortId,
        guest_mac: [u8; 6],
        uplink_signal: Arc<UplinkWorkSignal>,
    ) -> Arc<Self> {
        Arc::new(Self {
            id,
            guest_mac,
            egress: Mutex::new(VecDeque::new()),
            ingress: Mutex::new(VecDeque::new()),
            uplink_signal,
            guest_wake: WorkerWaitQueue::new(),
            ingress_ready: AtomicBool::new(false),
            active: AtomicBool::new(false),
            counters: PortCounters::default(),
        })
    }

    /// Marks the port live for this generation. Called once registration with
    /// the switch has succeeded and the delivery worker is starting.
    pub(super) fn activate(&self) {
        self.active.store(true, Ordering::Release);
    }

    /// Marks the port inert so the switch and workers stop using it. Teardown
    /// calls this before unregistering so in-flight frames are dropped instead
    /// of delivered to a going-away guest (design §8.2).
    pub(super) fn deactivate(&self) {
        self.active.store(false, Ordering::Release);
    }

    /// Event queue the guest delivery worker blocks on.
    pub fn guest_wake(&self) -> &WorkerWaitQueue {
        &self.guest_wake
    }

    /// Wakes a blocked guest delivery worker after an out-of-band change
    /// (shutdown, guest RX kick).
    pub fn wake_guest(&self) {
        self.guest_wake.wake_one();
    }

    // -- egress: guest TX -> uplink worker ---------------------------------

    fn try_push_egress(&self, frame: &[u8]) -> bool {
        let mut egress = self.egress.lock();
        if !self.is_active() {
            return false;
        }
        if egress.len() >= TX_QUEUE_CAPACITY {
            self.counters.egress_full.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        egress.push_back(frame.to_vec());
        true
    }

    /// Pops the next frame the uplink worker should classify and forward.
    pub(super) fn pop_egress(&self) -> Option<Vec<u8>> {
        self.egress.lock().pop_front()
    }

    /// Returns a frame that could not be host-transmitted (`Retry`) to the head
    /// of the egress queue so ordering is preserved (design §5.3).
    pub(super) fn requeue_egress(&self, frame: Vec<u8>) {
        self.egress.lock().push_front(frame);
    }

    /// Wakes the uplink worker because this port's egress went non-empty.
    fn signal_uplink(&self) {
        self.uplink_signal.signal();
    }

    // -- ingress: switch/host -> guest delivery worker ---------------------

    pub fn ingress_ready(&self) -> bool {
        self.ingress_ready.load(Ordering::Acquire)
    }

    pub fn clear_ingress_ready(&self) {
        self.ingress_ready.store(false, Ordering::Release);
    }

    pub fn drain_ingress(&self) -> Option<Vec<u8>> {
        self.ingress.lock().pop_front()
    }

    pub fn requeue_ingress(&self, frame: Vec<u8>) {
        self.ingress.lock().push_front(frame);
    }

    /// Notifies the delivery worker that the guest added RX buffers (kicked the
    /// RX virtqueue), so a previously-`NoGuestBuffer` frame can be retried.
    pub fn on_guest_rx_kick(&self) {
        self.ingress_ready.store(true, Ordering::Release);
        self.wake_guest();
    }

    /// Snapshot of the per-port counters (diagnostics/tests).
    #[cfg(test)]
    pub fn counters(&self) -> PortCountersSnapshot {
        PortCountersSnapshot {
            egress_full: self.counters.egress_full.load(Ordering::Relaxed),
            ingress_full: self.counters.ingress_full.load(Ordering::Relaxed),
        }
    }
}

impl SwitchPort for PortEndpoint {
    fn id(&self) -> SwitchPortId {
        self.id
    }

    fn guest_mac(&self) -> [u8; 6] {
        self.guest_mac
    }

    fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    fn deliver_ingress(&self, frame: &[u8]) -> bool {
        if !self.is_active() {
            return false;
        }
        let mut ingress = self.ingress.lock();
        if ingress.len() >= RX_QUEUE_CAPACITY {
            drop(ingress);
            self.counters.ingress_full.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        ingress.push_back(frame.to_vec());
        drop(ingress);
        self.ingress_ready.store(true, Ordering::Release);
        self.wake_guest();
        true
    }
}

/// Counters updated only on this port's queues; relaxed because they never gate
/// a decision (design §9.1).
#[derive(Default, Debug)]
struct PortCounters {
    egress_full: AtomicU64,
    ingress_full: AtomicU64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg(test)]
pub struct PortCountersSnapshot {
    pub egress_full: u64,
    pub ingress_full: u64,
}

/// Switch-backed backend held inside the device model.
#[derive(Clone)]
pub struct RawUplinkBackend {
    endpoint: Arc<PortEndpoint>,
}

impl NetworkBackend for RawUplinkBackend {
    fn transmit(&self, frame: &[u8]) -> Result<(), NetworkBackendError> {
        // Runs inside the device TX queue lock: bounded enqueue + signal only.
        // The uplink worker does the classification and host TX outside any
        // device lock (design §5.1, §7.5).
        if self.endpoint.try_push_egress(frame) {
            self.endpoint.signal_uplink();
            Ok(())
        } else {
            Err(NetworkBackendError::TransmitFailed)
        }
    }

    fn rx_queue_notified(&self) {
        self.endpoint.on_guest_rx_kick();
    }
}

/// Deterministic echo peer shared between the device (TX) and the RX worker.
#[derive(Clone)]
pub struct DeterministicUdpEchoBackend {
    shared: Arc<BackendShared>,
}

struct BackendShared {
    guest_mac: [u8; 6],
    rx_queue: Mutex<VecDeque<Vec<u8>>>,
    wake: WorkerWaitQueue,
    rx_ready: AtomicBool,
    stats: Mutex<BackendStats>,
}

#[derive(Default, Debug)]
struct BackendStats {
    arp_replies: u64,
    udp_echo_replies: u64,
    dropped_malformed: u64,
    dropped_unhandled: u64,
    dropped_full: u64,
}

/// Outcome of inspecting one transmitted frame.
enum TxOutcome {
    /// A reply was enqueued for the RX worker.
    Enqueued,
    /// The frame was dropped for the given diagnostic reason.
    Dropped(&'static str),
}

impl DeterministicUdpEchoBackend {
    /// Creates a new echo peer with an empty RX queue.
    pub fn new(guest_mac: [u8; 6]) -> Self {
        Self {
            shared: Arc::new(BackendShared {
                guest_mac,
                rx_queue: Mutex::new(VecDeque::new()),
                wake: WorkerWaitQueue::new(),
                rx_ready: AtomicBool::new(false),
                stats: Mutex::new(BackendStats::default()),
            }),
        }
    }

    /// Pops the next frame the peer wants delivered to the guest RX queue.
    pub fn drain_rx(&self) -> Option<Vec<u8>> {
        self.shared.rx_queue.lock().pop_front()
    }

    /// Returns whether at least one RX frame is buffered.
    #[cfg(test)]
    pub fn has_rx(&self) -> bool {
        !self.shared.rx_queue.lock().is_empty()
    }

    /// Returns whether an enqueue or guest RX kick made delivery retryable.
    pub fn rx_ready(&self) -> bool {
        self.shared.rx_ready.load(Ordering::Acquire)
    }

    /// Consumes the current delivery-readiness edge.
    pub fn clear_rx_ready(&self) {
        self.shared.rx_ready.store(false, Ordering::Release);
    }

    /// Restores a frame that could not be delivered because the guest had no buffer.
    pub fn requeue_rx(&self, frame: Vec<u8>) {
        self.shared.rx_queue.lock().push_front(frame);
    }

    /// Returns the event queue the RX worker blocks on.
    pub fn wake_queue(&self) -> &WorkerWaitQueue {
        &self.shared.wake
    }

    /// Wakes a blocked RX worker after out-of-band state changes (e.g. shutdown).
    pub fn wake_worker(&self) {
        self.shared.wake.wake_one();
    }

    /// Returns a snapshot of the backend counters (for diagnostics/tests).
    #[cfg(test)]
    pub fn stats(&self) -> BackendStatsSnapshot {
        let s = self.shared.stats.lock();
        BackendStatsSnapshot {
            arp_replies: s.arp_replies,
            udp_echo_replies: s.udp_echo_replies,
            dropped_malformed: s.dropped_malformed,
            dropped_unhandled: s.dropped_unhandled,
            dropped_full: s.dropped_full,
        }
    }

    fn handle_tx(&self, frame: &[u8]) -> TxOutcome {
        let reply = match classify(frame, self.shared.guest_mac) {
            Ok(Some(reply)) => reply,
            Ok(None) => {
                self.shared.stats.lock().dropped_unhandled += 1;
                return TxOutcome::Dropped("not ARP-for-peer or UDP-to-echo-port");
            }
            Err(reason) => {
                self.shared.stats.lock().dropped_malformed += 1;
                return TxOutcome::Dropped(reason);
            }
        };
        match reply {
            Reply::Arp(_) => self.shared.stats.lock().arp_replies += 1,
            Reply::UdpEcho(_) => self.shared.stats.lock().udp_echo_replies += 1,
        }

        let mut queue = self.shared.rx_queue.lock();
        if queue.len() >= RX_QUEUE_CAPACITY {
            drop(queue);
            self.shared.stats.lock().dropped_full += 1;
            TxOutcome::Dropped("RX backlog full")
        } else {
            queue.push_back(reply.into_frame());
            drop(queue);
            self.shared.rx_ready.store(true, Ordering::Release);
            TxOutcome::Enqueued
        }
    }
}

impl NetworkBackend for DeterministicUdpEchoBackend {
    fn transmit(&self, frame: &[u8]) -> Result<(), NetworkBackendError> {
        match self.handle_tx(frame) {
            TxOutcome::Enqueued => self.shared.wake.wake_one(),
            TxOutcome::Dropped(reason) => debug!("virtio-net echo peer dropped frame: {reason}"),
        }
        // TX always completes from the device's perspective; peer-side drops are
        // recorded as counters, not surfaced as backend errors.
        Ok(())
    }

    fn rx_queue_notified(&self) {
        self.shared.rx_ready.store(true, Ordering::Release);
        self.shared.wake.wake_one();
    }
}

/// Read-only snapshot of [`BackendStats`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg(test)]
pub struct BackendStatsSnapshot {
    pub arp_replies: u64,
    pub udp_echo_replies: u64,
    pub dropped_malformed: u64,
    pub dropped_unhandled: u64,
    pub dropped_full: u64,
}

// ---------------------------------------------------------------------------
// Frame classification and deterministic reply construction.
// ---------------------------------------------------------------------------

const ETHERNET_HEADER_LEN: usize = 14;
const ETH_TYPE_ARP: [u8; 2] = [0x08, 0x06];
const ETH_TYPE_IPV4: [u8; 2] = [0x08, 0x00];
const IP_PROTO_UDP: u8 = 17;
const ARP_ETHERNET_IPV4: [u8; 6] = [0x00, 0x01, 0x08, 0x00, 0x06, 0x04];
const ARP_OP_REQUEST: [u8; 2] = [0x00, 0x01];

enum Reply {
    Arp(Vec<u8>),
    UdpEcho(Vec<u8>),
}

impl Reply {
    fn into_frame(self) -> Vec<u8> {
        match self {
            Reply::Arp(frame) | Reply::UdpEcho(frame) => frame,
        }
    }
}

/// Inspects `frame` and, if it is an ARP request for the peer or a UDP echo
/// datagram, returns the deterministic reply to enqueue.
///
/// - `Ok(Some(reply))`: a reply should be enqueued.
/// - `Ok(None)`: the frame is well-formed but not one the peer answers.
/// - `Err(reason)`: the frame is too short or malformed to classify.
fn classify(frame: &[u8], guest_mac: [u8; 6]) -> Result<Option<Reply>, &'static str> {
    let eth = parse_ethernet(frame)?;
    if eth.src != guest_mac {
        return Err("Ethernet source does not match configured guest MAC");
    }
    Ok(match eth.ethertype {
        ETH_TYPE_ARP => build_arp_reply(frame, eth, guest_mac).map(Reply::Arp),
        ETH_TYPE_IPV4 => build_udp_echo_reply(frame, eth).map(Reply::UdpEcho),
        _ => None,
    })
}

struct EthernetHeader {
    dst: [u8; 6],
    src: [u8; 6],
    ethertype: [u8; 2],
}

fn parse_ethernet(frame: &[u8]) -> Result<EthernetHeader, &'static str> {
    if frame.len() < ETHERNET_HEADER_LEN {
        return Err("frame shorter than Ethernet header");
    }
    let mut dst = [0u8; 6];
    let mut src = [0u8; 6];
    dst.copy_from_slice(&frame[0..6]);
    src.copy_from_slice(&frame[6..12]);
    Ok(EthernetHeader {
        dst,
        src,
        ethertype: [frame[12], frame[13]],
    })
}

/// Builds an ARP reply if `frame` is an ARP request for [`PEER_IPV4`].
fn build_arp_reply(frame: &[u8], eth: EthernetHeader, guest_mac: [u8; 6]) -> Option<Vec<u8>> {
    // ARP packet for Ethernet/IPv4 is 28 bytes after the 14-byte Ethernet header.
    const ARP_LEN: usize = 28;
    if frame.len() < ETHERNET_HEADER_LEN + ARP_LEN {
        return None;
    }
    let arp = &frame[ETHERNET_HEADER_LEN..ETHERNET_HEADER_LEN + ARP_LEN];
    if arp[0..6] != ARP_ETHERNET_IPV4 || arp[6..8] != ARP_OP_REQUEST {
        return None;
    }
    // Target protocol address is bytes 24..28 of the ARP packet.
    if arp[24..28] != PEER_IPV4
        || arp[8..14] != guest_mac
        || arp[14..18] != GUEST_IPV4
        || (eth.dst != [0xff; 6] && eth.dst != PEER_MAC)
    {
        return None;
    }
    // Sender protocol address (the guest IPv4) is bytes 14..18.
    let sender_ip: [u8; 4] = arp[14..18].try_into().ok()?;

    let mut reply = Vec::with_capacity(ETHERNET_HEADER_LEN + ARP_LEN);
    // Ethernet: from peer -> original sender.
    reply.extend_from_slice(&eth.src);
    reply.extend_from_slice(&PEER_MAC);
    reply.extend_from_slice(&ETH_TYPE_ARP);
    // ARP: reply (op=2), sender=peer, target=guest.
    reply.extend_from_slice(&ARP_ETHERNET_IPV4);
    reply.extend_from_slice(&[0x00, 0x02]); // ARP reply
    reply.extend_from_slice(&PEER_MAC);
    reply.extend_from_slice(&PEER_IPV4);
    reply.extend_from_slice(&eth.src);
    reply.extend_from_slice(&sender_ip);
    Some(reply)
}

/// Builds a UDP echo reply if `frame` is a UDP datagram to [`PEER_IPV4`] on
/// [`ECHO_UDP_PORT`]. Swaps addresses/ports and echoes the payload verbatim.
fn build_udp_echo_reply(frame: &[u8], eth: EthernetHeader) -> Option<Vec<u8>> {
    let ip = parse_ipv4_header(frame)?;
    if eth.dst != PEER_MAC
        || ip.proto != IP_PROTO_UDP
        || ip.src != GUEST_IPV4
        || ip.dst != PEER_IPV4
    {
        return None;
    }
    let udp_bytes = frame.get(ip.header_len..ip.total_len)?;
    let udp = parse_udp_header(udp_bytes)?;
    if udp.dst_port != ECHO_UDP_PORT
        || (udp.checksum != 0 && udp_ipv4_checksum(ip.src, ip.dst, udp_bytes) != 0)
    {
        return None;
    }
    let payload_start = ip.header_len + 8;
    let payload_end = ip.header_len + udp.len;
    let payload = frame.get(payload_start..payload_end)?;

    let mut reply = Vec::with_capacity(ETHERNET_HEADER_LEN + 20 + 8 + payload.len());
    // Ethernet: peer -> guest.
    reply.extend_from_slice(&eth.src);
    reply.extend_from_slice(&PEER_MAC);
    reply.extend_from_slice(&ETH_TYPE_IPV4);
    // IPv4 header (20 bytes, no options). Checksum computed below.
    let ipv4_offset = reply.len();
    let total_len = (20 + 8 + payload.len()) as u16;
    reply.extend_from_slice(&[0x45, 0x00]); // version=4, ihl=5; tos=0
    reply.extend_from_slice(&total_len.to_be_bytes());
    reply.extend_from_slice(&[0, 0, 0x40, 0, 64, IP_PROTO_UDP]); // id, flags/frag, ttl, proto
    reply.extend_from_slice(&[0, 0]); // checksum placeholder
    reply.extend_from_slice(&PEER_IPV4); // src = peer
    reply.extend_from_slice(&ip.src); // dst = guest
    let checksum = ipv4_checksum(&reply[ipv4_offset..ipv4_offset + 20]);
    reply[ipv4_offset + 10..ipv4_offset + 12].copy_from_slice(&checksum.to_be_bytes());
    // UDP header. Source = echo port, dest = original source port.
    let udp_offset = reply.len();
    reply.extend_from_slice(&ECHO_UDP_PORT.to_be_bytes());
    reply.extend_from_slice(&udp.src_port.to_be_bytes());
    let udp_len = (8 + payload.len()) as u16;
    reply.extend_from_slice(&udp_len.to_be_bytes());
    reply.extend_from_slice(&[0, 0]);
    reply.extend_from_slice(payload);
    let checksum = udp_ipv4_checksum(PEER_IPV4, ip.src, &reply[udp_offset..]);
    let checksum = if checksum == 0 { u16::MAX } else { checksum };
    reply[udp_offset + 6..udp_offset + 8].copy_from_slice(&checksum.to_be_bytes());
    Some(reply)
}

struct Ipv4Header {
    header_len: usize,
    total_len: usize,
    proto: u8,
    src: [u8; 4],
    dst: [u8; 4],
}

fn parse_ipv4_header(frame: &[u8]) -> Option<Ipv4Header> {
    if frame.len() < ETHERNET_HEADER_LEN + 20 {
        return None;
    }
    let ip = &frame[ETHERNET_HEADER_LEN..];
    let version_ihl = ip[0];
    if version_ihl >> 4 != 4 {
        return None;
    }
    let ihl = (version_ihl & 0x0f) as usize;
    if ihl < 5 {
        return None;
    }
    let header_len = ihl * 4;
    let total_len = u16::from_be_bytes([ip[2], ip[3]]) as usize;
    if total_len < header_len || frame.len() < ETHERNET_HEADER_LEN + total_len {
        return None;
    }
    let fragment = u16::from_be_bytes([ip[6], ip[7]]);
    if fragment & 0x3fff != 0 || ipv4_checksum(&ip[..header_len]) != 0 {
        return None;
    }
    let mut src = [0u8; 4];
    let mut dst = [0u8; 4];
    src.copy_from_slice(&ip[12..16]);
    dst.copy_from_slice(&ip[16..20]);
    Some(Ipv4Header {
        header_len: ETHERNET_HEADER_LEN + header_len,
        total_len: ETHERNET_HEADER_LEN + total_len,
        proto: ip[9],
        src,
        dst,
    })
}

struct UdpHeader {
    src_port: u16,
    dst_port: u16,
    len: usize,
    checksum: u16,
}

fn parse_udp_header(ip_payload: &[u8]) -> Option<UdpHeader> {
    if ip_payload.len() < 8 {
        return None;
    }
    let len = u16::from_be_bytes([ip_payload[4], ip_payload[5]]) as usize;
    if len < 8 || len != ip_payload.len() {
        return None;
    }
    Some(UdpHeader {
        src_port: u16::from_be_bytes([ip_payload[0], ip_payload[1]]),
        dst_port: u16::from_be_bytes([ip_payload[2], ip_payload[3]]),
        len,
        checksum: u16::from_be_bytes([ip_payload[6], ip_payload[7]]),
    })
}

/// Computes the IPv4 header checksum (ones-complement sum of 16-bit words).
fn ipv4_checksum(header: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    for chunk in header.chunks_exact(2) {
        sum += u16::from_be_bytes([chunk[0], chunk[1]]) as u32;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

fn udp_ipv4_checksum(src: [u8; 4], dst: [u8; 4], datagram: &[u8]) -> u16 {
    let mut sum = 0u32;
    for chunk in src.chunks_exact(2).chain(dst.chunks_exact(2)) {
        sum += u16::from_be_bytes([chunk[0], chunk[1]]) as u32;
    }
    sum += IP_PROTO_UDP as u32;
    sum += datagram.len() as u32;
    for chunk in datagram.chunks_exact(2) {
        sum += u16::from_be_bytes([chunk[0], chunk[1]]) as u32;
    }
    if let Some(byte) = datagram.chunks_exact(2).remainder().first() {
        sum += (*byte as u32) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GUEST_MAC: [u8; 6] = [0x52, 0x54, 0, 0x12, 0x34, 0x56];

    fn udp_frame() -> Vec<u8> {
        let payload = b"echo-token";
        let mut frame = Vec::new();
        frame.extend_from_slice(&PEER_MAC);
        frame.extend_from_slice(&GUEST_MAC);
        frame.extend_from_slice(&ETH_TYPE_IPV4);
        let ip_offset = frame.len();
        let total_len = (20 + 8 + payload.len()) as u16;
        frame.extend_from_slice(&[0x45, 0]);
        frame.extend_from_slice(&total_len.to_be_bytes());
        frame.extend_from_slice(&[0, 1, 0x40, 0, 64, IP_PROTO_UDP]);
        frame.extend_from_slice(&[0, 0]);
        frame.extend_from_slice(&GUEST_IPV4);
        frame.extend_from_slice(&PEER_IPV4);
        let ip_checksum = ipv4_checksum(&frame[ip_offset..ip_offset + 20]);
        frame[ip_offset + 10..ip_offset + 12].copy_from_slice(&ip_checksum.to_be_bytes());
        frame.extend_from_slice(&12345u16.to_be_bytes());
        frame.extend_from_slice(&ECHO_UDP_PORT.to_be_bytes());
        frame.extend_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
        frame.extend_from_slice(&[0, 0]);
        frame.extend_from_slice(payload);
        frame
    }

    #[test]
    fn valid_udp_frame_enqueues_checksummed_reply() {
        let backend = DeterministicUdpEchoBackend::new(GUEST_MAC);

        backend.transmit(&udp_frame()).unwrap();

        let reply = backend.drain_rx().unwrap();
        let ip = parse_ipv4_header(&reply).unwrap();
        let udp = &reply[ip.header_len..ip.total_len];
        assert_eq!(reply[..6], GUEST_MAC);
        assert_eq!(udp_ipv4_checksum(PEER_IPV4, GUEST_IPV4, udp), 0);
    }

    #[test]
    fn rejects_wrong_source_mac_fragment_and_bad_udp_length() {
        let backend = DeterministicUdpEchoBackend::new(GUEST_MAC);
        let mut wrong_mac = udp_frame();
        wrong_mac[6] ^= 1;
        backend.transmit(&wrong_mac).unwrap();

        let mut fragmented = udp_frame();
        fragmented[ETHERNET_HEADER_LEN + 6] = 0x20;
        fragmented[ETHERNET_HEADER_LEN + 10..ETHERNET_HEADER_LEN + 12].fill(0);
        let checksum = ipv4_checksum(&fragmented[ETHERNET_HEADER_LEN..ETHERNET_HEADER_LEN + 20]);
        fragmented[ETHERNET_HEADER_LEN + 10..ETHERNET_HEADER_LEN + 12]
            .copy_from_slice(&checksum.to_be_bytes());
        backend.transmit(&fragmented).unwrap();

        let mut bad_udp_len = udp_frame();
        let udp_len = ETHERNET_HEADER_LEN + 20 + 4;
        bad_udp_len[udp_len..udp_len + 2].copy_from_slice(&8u16.to_be_bytes());
        backend.transmit(&bad_udp_len).unwrap();

        assert!(backend.drain_rx().is_none());
        assert_eq!(backend.stats().dropped_malformed, 1);
        assert_eq!(backend.stats().dropped_unhandled, 2);
    }
}
