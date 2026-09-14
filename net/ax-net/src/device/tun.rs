//! TUN/TAP devices driven by `/dev/net/tun`.
//!
//! Userspace is the link. A TUN exchanges bare IP packets and a TAP exchanges
//! Ethernet frames with the attached file through two bounded queues. Like
//! loopback, these devices stay outside the IRQ-backed queue runtime: a write
//! publishes protocol work with [`crate::request_poll`], and the router polls
//! the device directly.

use alloc::{boxed::Box, collections::VecDeque, string::String, sync::Arc};
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use ax_sync::SpinLock;
use axpoll::IoEvents;
use axpoll_set::PollSet;
use smoltcp::{storage::PacketBuffer, time::Instant, wire::IpAddress};

use crate::{
    config::InterfaceId,
    consts::STANDARD_MTU,
    device::{
        Device, EthernetDevice, EthernetFramePort, NetDeviceError, NetDeviceResult,
        ProtocolEthernetFrame,
    },
};

/// Frames buffered per direction, Linux `TUN_READQ_SIZE`.
const TUN_QUEUE_LEN: usize = 500;

/// `ETH_HLEN`, the link header a TAP carries on top of its MTU.
const ETH_HLEN: usize = 14;

struct TunQueue(SpinLock<VecDeque<Box<[u8]>>>);

impl TunQueue {
    const fn new() -> Self {
        Self(SpinLock::new(VecDeque::new()))
    }

    /// Appends a frame, or returns `false` when the queue is full.
    fn push_back(&self, frame: &[u8]) -> bool {
        let frame = Box::from(frame);
        let mut queue = self.0.lock_irqsave();
        if queue.len() >= TUN_QUEUE_LEN {
            return false;
        }
        queue.push_back(frame);
        true
    }

    fn push_front(&self, frame: Box<[u8]>) {
        self.0.lock_irqsave().push_front(frame);
    }

    fn pop_front(&self) -> Option<Box<[u8]>> {
        self.0.lock_irqsave().pop_front()
    }

    fn is_empty(&self) -> bool {
        self.0.lock_irqsave().is_empty()
    }

    fn clear(&self) {
        self.0.lock_irqsave().clear();
    }
}

/// Owner of the interface's single queue.
///
/// `Dying` is terminal, so a `TUNSETIFF` racing the last close cannot attach a
/// device that is about to be unregistered.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AttachState {
    Free,
    Attached,
    Dying,
}

/// State shared by the router-side device and the `/dev/net/tun` file.
pub struct TunShared {
    name: String,
    /// Frames written by userspace, drained by the protocol executor.
    rx: TunQueue,
    /// Frames the stack routed to the interface, drained by `read(2)`.
    tx: TunQueue,
    readers: Arc<PollSet>,
    attach: SpinLock<AttachState>,
    /// Device-level `IFF_PERSIST`, shared by every file that attaches.
    persist: AtomicBool,
    up: AtomicBool,
    /// MTU plus link header, the largest frame handed to `read(2)`.
    frame_limit: AtomicUsize,
    header_len: usize,
    rx_dropped: AtomicU64,
    tx_dropped: AtomicU64,
}

impl TunShared {
    fn new(name: String, header_len: usize) -> Arc<Self> {
        Arc::new(Self {
            name,
            rx: TunQueue::new(),
            tx: TunQueue::new(),
            readers: Arc::new(PollSet::new()),
            attach: SpinLock::new(AttachState::Free),
            persist: AtomicBool::new(false),
            up: AtomicBool::new(false),
            frame_limit: AtomicUsize::new(STANDARD_MTU + header_len),
            header_len,
            rx_dropped: AtomicU64::new(0),
            tx_dropped: AtomicU64::new(0),
        })
    }

    /// Creates state that is not registered with the network service and
    /// carries no traffic, for exercising the attach state machine.
    pub fn detached(name: String) -> Arc<Self> {
        Self::new(name, 0)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Claims the single queue, as Linux `tun_attach` does for a device without
    /// `IFF_MULTI_QUEUE`.
    pub fn try_attach(&self) -> bool {
        let mut state = self.attach.lock_irqsave();
        let free = *state == AttachState::Free;
        if free {
            *state = AttachState::Attached;
        }
        free
    }

    /// Releases the queue unless the device is dying. Frames nobody read are
    /// dropped, as `__tun_detach` purges the read queue.
    pub fn detach(&self) {
        {
            let mut state = self.attach.lock_irqsave();
            if *state == AttachState::Attached {
                *state = AttachState::Free;
            }
        }
        self.tx.clear();
    }

    /// Makes every later [`try_attach`](Self::try_attach) fail.
    pub fn mark_dying(&self) {
        *self.attach.lock_irqsave() = AttachState::Dying;
    }

    pub fn is_attached(&self) -> bool {
        *self.attach.lock_irqsave() == AttachState::Attached
    }

    pub fn is_dying(&self) -> bool {
        *self.attach.lock_irqsave() == AttachState::Dying
    }

    pub fn is_persistent(&self) -> bool {
        self.persist.load(Ordering::Acquire)
    }

    pub fn set_persist(&self, persist: bool) {
        self.persist.store(persist, Ordering::Release);
    }

    /// Whether the interface is administratively up (`IFF_UP`).
    pub fn is_up(&self) -> bool {
        self.up.load(Ordering::Acquire)
    }

    pub(crate) fn set_up(&self, up: bool) {
        self.up.store(up, Ordering::Release);
    }

    pub(crate) fn set_mtu(&self, mtu: usize) {
        self.frame_limit
            .store(mtu + self.header_len, Ordering::Release);
    }

    /// Injects a frame written by userspace. Linux `tun_get_user` does not check
    /// the MTU on this path; a frame the protocol buffers cannot hold, or one
    /// arriving with the backlog full, is dropped without failing the write.
    pub fn push_rx(&self, frame: &[u8]) {
        if frame.len() <= STANDARD_MTU + self.header_len && self.rx.push_back(frame) {
            crate::request_poll();
        } else {
            self.rx_dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Takes the next frame routed to the interface.
    pub fn pop_tx(&self) -> Option<Box<[u8]>> {
        self.tx.pop_front()
    }

    pub fn has_tx(&self) -> bool {
        !self.tx.is_empty()
    }

    /// Readers blocked on routed frames.
    pub fn readers(&self) -> &PollSet {
        &self.readers
    }

    /// Queues a routed frame for `read(2)` and wakes its readers.
    fn push_tx(&self, frame: &[u8]) -> bool {
        let queued = self.enqueue_tx(frame);
        if queued {
            crate::defer_poll_wake(self.readers.clone(), IoEvents::IN);
        }
        queued
    }

    /// Like `tun_net_xmit`, a frame above the MTU or a full queue drops the
    /// frame instead of stalling egress.
    fn enqueue_tx(&self, frame: &[u8]) -> bool {
        if frame.len() > self.frame_limit.load(Ordering::Acquire) || !self.tx.push_back(frame) {
            self.tx_dropped.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        true
    }
}

/// Router-side half of a TUN interface.
pub(crate) struct TunDevice {
    shared: Arc<TunShared>,
}

impl Device for TunDevice {
    fn name(&self) -> &str {
        self.shared.name()
    }

    fn recv(
        &mut self,
        interface_id: InterfaceId,
        buffer: &mut PacketBuffer<InterfaceId>,
        _timestamp: Instant,
        snoop: &mut dyn FnMut(&[u8]),
    ) -> usize {
        let Some(packet) = self.shared.rx.pop_front() else {
            return 0;
        };
        let Ok(dst) = buffer.enqueue(packet.len(), interface_id) else {
            self.shared.rx.push_front(packet);
            return 0;
        };
        snoop(&packet);
        dst.copy_from_slice(&packet);
        packet.len()
    }

    fn recv_direct(
        &mut self,
        _timestamp: Instant,
        deliver: &mut dyn FnMut(&[u8]) -> bool,
        snoop: &mut dyn FnMut(&[u8]),
    ) -> Option<usize> {
        let Some(packet) = self.shared.rx.pop_front() else {
            return Some(0);
        };
        if !deliver(&packet) {
            self.shared.rx.push_front(packet);
            return Some(0);
        }
        snoop(&packet);
        Some(packet.len())
    }

    fn send(&mut self, _next_hop: IpAddress, packet: &[u8], _timestamp: Instant) -> usize {
        if self.shared.push_tx(packet) {
            packet.len()
        } else {
            0
        }
    }

    fn drain_deferred_tx_drops(&mut self) -> u64 {
        self.shared.tx_dropped.swap(0, Ordering::Relaxed)
    }

    fn drain_deferred_rx_drops(&mut self) -> u64 {
        self.shared.rx_dropped.swap(0, Ordering::Relaxed)
    }
}

/// Ethernet frame port whose "wire" is a TAP file's queues.
struct TapPort {
    shared: Arc<TunShared>,
    mac: [u8; 6],
}

impl EthernetFramePort for TapPort {
    fn device_name(&self) -> &str {
        self.shared.name()
    }

    fn mac_address(&self) -> [u8; 6] {
        self.mac
    }

    fn transmit(&mut self, frame: &ProtocolEthernetFrame) -> NetDeviceResult {
        self.shared.push_tx(frame.packet());
        Ok(())
    }

    fn receive(&mut self) -> NetDeviceResult<ProtocolEthernetFrame> {
        let frame = self.shared.rx.pop_front().ok_or(NetDeviceError::Again)?;
        ProtocolEthernetFrame::copy_from_slice(&frame)
    }
}

/// A locally administered unicast MAC derived from the interface id; Linux
/// picks a random one when it sets up a TAP device.
pub(crate) fn tap_mac(interface_id: InterfaceId) -> [u8; 6] {
    let [a, b, c, d] = interface_id.get().to_be_bytes();
    [0x02, 0x00, a, b, c, d]
}

/// Creates a TUN router device and the state its file keeps.
pub(crate) fn create_tun(name: String) -> (TunDevice, Arc<TunShared>) {
    let shared = TunShared::new(name, 0);
    (
        TunDevice {
            shared: shared.clone(),
        },
        shared,
    )
}

/// Creates a TAP interface: the Ethernet adapter over a [`TapPort`].
pub(crate) fn create_tap(name: String, mac: [u8; 6]) -> (EthernetDevice, Arc<TunShared>) {
    let shared = TunShared::new(name.clone(), ETH_HLEN);
    let port = TapPort {
        shared: shared.clone(),
        mac,
    };
    let mut device = EthernetDevice::new(name, Box::new(port), None);
    // `eth_type_trans` passes multicast frames up; unicast frames for another
    // host become `PACKET_OTHERHOST` and IP/ARP drop them.
    device.set_accept_multicast(true);
    (device, shared)
}

#[cfg(test)]
mod tests {
    use alloc::{string::ToString, vec::Vec};

    use super::*;

    #[test]
    fn attach_state_follows_tun_attach() {
        let shared = TunShared::detached("tun0".to_string());
        assert!(shared.try_attach());
        assert!(!shared.try_attach(), "a second queue must be refused");
        shared.detach();
        assert!(shared.try_attach(), "a detached device can be reattached");
        shared.mark_dying();
        shared.detach();
        assert!(shared.is_dying());
        assert!(!shared.try_attach(), "a dying device never attaches");
    }

    #[test]
    fn queues_drop_at_capacity() {
        let queue = TunQueue::new();
        for _ in 0..TUN_QUEUE_LEN {
            assert!(queue.push_back(&[0x45]));
        }
        assert!(!queue.push_back(&[0x45]));
        assert!(queue.pop_front().is_some());
        assert!(queue.push_back(&[0x45]));
    }

    #[test]
    fn oversized_writes_are_dropped() {
        let shared = TunShared::detached("tun0".to_string());
        shared.push_rx(&[0u8; STANDARD_MTU + 1]);
        assert!(shared.rx.is_empty());
        assert_eq!(shared.rx_dropped.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn undelivered_packets_stay_queued() {
        let (mut device, shared) = create_tun("tun0".to_string());
        assert!(shared.rx.push_back(&[0x45, 1]));
        assert!(shared.rx.push_back(&[0x45, 2]));
        let mut delivered = Vec::new();
        let mut refuse = |_: &[u8]| false;
        assert_eq!(
            device.recv_direct(Instant::ZERO, &mut refuse, &mut |_| {}),
            Some(0)
        );
        let mut accept = |packet: &[u8]| {
            delivered.push(packet.to_vec());
            true
        };
        assert_eq!(
            device.recv_direct(Instant::ZERO, &mut accept, &mut |_| {}),
            Some(2)
        );
        assert_eq!(delivered, [alloc::vec![0x45, 1]]);
    }

    #[test]
    fn routed_frames_respect_the_mtu() {
        let shared = TunShared::detached("tun0".to_string());
        shared.set_mtu(1280);
        assert!(!shared.enqueue_tx(&[0u8; 1281]));
        assert!(shared.enqueue_tx(&[0u8; 1280]));
        assert_eq!(shared.pop_tx().map(|frame| frame.len()), Some(1280));
        assert_eq!(shared.tx_dropped.load(Ordering::Relaxed), 1);
    }
}
