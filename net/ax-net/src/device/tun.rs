//! TUN/TAP virtual device adapter.
//!
//! A TUN device bridges the single IP-medium protocol core to a userspace
//! `/dev/net/tun` file descriptor. Unlike Ethernet it owns no driver and no ARP
//! state: userspace *is* the link. Packets flow through two bounded queues that
//! decouple the char-device fd (arbitrary syscall contexts) from the router's
//! per-device RX/TX workers.
//!
//! # Data Path
//!
//! - `write(2)` on the fd pushes a bare IP packet into [`TunShared::rx_queue`]
//!   and wakes the poll set with `IN`. The router's RX worker has armed its own
//!   waker on that poll set, so it runs [`Device::recv`], which drains the queue
//!   into the shared smoltcp RX buffer.
//! - The router selects this device for an outbound packet and calls
//!   [`Device::send`], which pushes onto [`TunShared::tx_queue`] and wakes the
//!   poll set with `IN` so a blocked `read(2)`/`poll(2)` on the fd wakes up.
//!
//! The char device holds an [`Arc<TunShared>`] obtained at creation time, so it
//! reads and writes the queues without ever taking the router's device lock.

use alloc::{boxed::Box, collections::VecDeque, string::String, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use ax_sync::SpinLock;
use axpoll::{IoEvents, PollSet};
use irq_framework::IrqId;
use smoltcp::{
    storage::PacketBuffer,
    time::Instant,
    wire::{IpAddress, Ipv4Cidr},
};

use crate::{
    config::InterfaceId,
    consts::{STACK_MAX_MTU, STANDARD_MTU},
    device::{
        Device, EthernetDevice, EthernetDriver, NetDeviceError, NetDeviceResult, NetIrqEvents,
        NetRxBuffer, NetTxBuffer,
    },
};

/// Upper bound on packets buffered in each direction. Matches the router's
/// per-device TX queue depth so a burst of writes cannot grow memory without
/// bound; excess packets are dropped exactly like a full hardware ring.
const TUN_QUEUE_CAPACITY: usize = 256;

/// Ethernet header bytes prepended to every layer-2 TAP frame.
const ETHERNET_HEADER_LEN: usize = 14;

/// Largest frame a queue slot accepts. A layer-3 TUN carries a bare IP packet
/// capped at the MTU; a layer-2 TAP prepends a 14-byte Ethernet header. The
/// ceiling matches Linux's TUN/TAP `MAX_MTU` (`drivers/net/tun.c`), so a
/// datagram sized within a configured interface MTU is never rejected here.
const TUN_FRAME_CAPACITY: usize = STACK_MAX_MTU + ETHERNET_HEADER_LEN;

/// Owned frame held in a TUN/TAP queue.
///
/// The frame is heap-backed and sized to its actual length, so the queue's
/// memory ceiling is `depth × actual-frame-size` rather than `depth ×
/// TUN_FRAME_CAPACITY`; raising an interface's MTU costs memory only for the
/// large frames actually in flight. Mirrors the router's `QueuedPacket`.
struct TunPacket {
    bytes: Box<[u8]>,
}

impl TunPacket {
    fn new(packet: &[u8]) -> Option<Self> {
        if packet.len() > TUN_FRAME_CAPACITY {
            return None;
        }
        Some(Self {
            bytes: Box::from(packet),
        })
    }

    fn as_slice(&self) -> &[u8] {
        &self.bytes
    }
}

/// Bounded FIFO shared between the char fd and a router worker.
///
/// A drop-oldest policy on overflow keeps a slow reader from wedging the whole
/// device: Linux's `tun` driver drops on a full ring rather than blocking the
/// stack, so incoming stack packets never stall on an absent userspace reader.
struct TunQueue {
    inner: SpinLock<VecDeque<TunPacket>>,
    len: AtomicUsize,
}

impl TunQueue {
    fn new() -> Self {
        Self {
            inner: SpinLock::new(VecDeque::new()),
            len: AtomicUsize::new(0),
        }
    }

    /// Pushes `packet`, dropping the oldest entry when full. Returns `false`
    /// when the packet exceeds the MTU and was rejected outright.
    fn push(&self, packet: &[u8]) -> bool {
        let Some(entry) = TunPacket::new(packet) else {
            return false;
        };
        let mut inner = self.inner.lock_irqsave();
        if inner.len() >= TUN_QUEUE_CAPACITY {
            inner.pop_front();
        }
        inner.push_back(entry);
        self.len.store(inner.len(), Ordering::Release);
        true
    }

    fn pop(&self) -> Option<TunPacket> {
        let mut inner = self.inner.lock_irqsave();
        let packet = inner.pop_front();
        self.len.store(inner.len(), Ordering::Release);
        packet
    }

    fn is_empty(&self) -> bool {
        self.len.load(Ordering::Acquire) == 0
    }
}

/// Lifecycle state of the single-queue attachment slot.
///
/// Transitions (all under `TunShared::attach_state` lock):
/// - `Free → Attached`: `try_attach()` succeeds
/// - `Attached → Free`: `detach()` on a non-dying device
/// - `Free | Attached → Dying`: `mark_dying()` before `destroy_tun()`
///
/// `Dying` is terminal: once set, `try_attach()` always returns false so no
/// fd can acquire the device after `destroy_tun()` is called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttachState {
    Free,
    Attached,
    Dying,
}

/// State shared between the [`TunDevice`] living inside the router and the
/// `/dev/net/tun` char device that drives it.
pub struct TunShared {
    /// Interface name, e.g. `tun0`. Fixed once the device is created.
    name: String,
    /// User `write(2)` -> stack. Drained by [`TunDevice::recv`].
    rx_queue: TunQueue,
    /// Stack -> user `read(2)`. Filled by [`TunDevice::send`].
    tx_queue: TunQueue,
    /// Readiness for both the router RX worker (armed via `readiness_poll`) and
    /// blocked char-device readers/`poll(2)`.
    poll_set: Arc<PollSet>,
    /// Attachment lifecycle state. Guards the single-queue slot against
    /// concurrent `TUNSETIFF`/`close`/`destroy_tun` races. This driver does
    /// not implement `IFF_MULTI_QUEUE`, so at most one fd may be attached at a
    /// time; Linux `tun_attach` returns `EBUSY` for a second queue on a
    /// non-multi-queue device.
    attach_state: SpinLock<AttachState>,
    /// Device-level persist flag (`IFF_PERSIST` in Linux `tun_struct::flags`).
    /// When set, the interface survives after the last fd closes and can be
    /// re-attached by a subsequent `TUNSETIFF`. Set/cleared by `TUNSETPERSIST`.
    persist: AtomicBool,
    /// Largest *frame* this interface admits on either queue: the configured MTU
    /// adjusted for link-layer overhead. For a layer-3 TUN the frame is the bare
    /// IP datagram, so the limit is `mtu`; for a layer-2 TAP the frame carries a
    /// 14-byte Ethernet header, so the MTU bounds the payload and the limit is
    /// `mtu + ETHERNET_HEADER_LEN`. Updated by `SIOCSIFMTU`. Mirrors Linux
    /// `dev->mtu` combined with `hard_header_len` from `tun_net_init`
    /// (drivers/net/tun.c): 0 overhead for IFF_TUN, `ETH_HLEN` for IFF_TAP.
    frame_limit: AtomicUsize,
    /// Link-layer header bytes added on top of the MTU to form `frame_limit`:
    /// 0 for TUN, `ETHERNET_HEADER_LEN` for TAP. Lets the `SIOCSIFMTU` setter
    /// recompute `frame_limit` from a new MTU without re-plumbing the kind.
    l2_overhead: usize,
}

impl TunShared {
    fn new(name: String, l2_overhead: usize) -> Arc<Self> {
        Arc::new(Self {
            name,
            rx_queue: TunQueue::new(),
            tx_queue: TunQueue::new(),
            poll_set: Arc::new(PollSet::new()),
            attach_state: SpinLock::new(AttachState::Free),
            persist: AtomicBool::new(false),
            // Seed with the creation-time default MTU, matching
            // `Service::create_tun`, which files every new interface at
            // `mtu: STANDARD_MTU`. A later `SIOCSIFMTU` overwrites it.
            frame_limit: AtomicUsize::new(STANDARD_MTU + l2_overhead),
            l2_overhead,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Claims the interface's single queue for a `/dev/net/tun` fd. Returns
    /// `false` when another fd already owns it (matching Linux `tun_attach`'s
    /// `EBUSY` for a second queue on a non-multi-queue device), or when the
    /// device is being destroyed (`Dying` state).
    pub fn try_attach(&self) -> bool {
        let mut state = self.attach_state.lock_irqsave();
        if *state == AttachState::Free {
            *state = AttachState::Attached;
            true
        } else {
            false
        }
    }

    /// Releases the queue when the owning fd detaches (close, or persist
    /// toggle), so a later `TUNSETIFF` on a persistent interface can reattach.
    /// Has no effect when the device is in the `Dying` state.
    pub fn detach(&self) {
        let mut state = self.attach_state.lock_irqsave();
        if *state == AttachState::Attached {
            *state = AttachState::Free;
        }
    }

    /// Marks the device as dying, preventing any future `try_attach()` from
    /// succeeding. Call this before `destroy_tun()` to close the TOCTOU window
    /// between `detach()` and device removal: a concurrent `TUNSETIFF` that
    /// finds the device by name will fail `try_attach()` and return `EBUSY`
    /// rather than acquiring a handle to a device that is about to vanish.
    pub fn mark_dying(&self) {
        *self.attach_state.lock_irqsave() = AttachState::Dying;
    }

    /// Returns whether this device is persistent (`IFF_PERSIST`). A persistent
    /// device survives after the last fd closes and can be re-attached by a
    /// later `TUNSETIFF` on the same name.
    pub fn is_persistent(&self) -> bool {
        self.persist.load(Ordering::Acquire)
    }

    /// Sets the `IFF_PERSIST` flag on this device. Corresponds to Linux
    /// `TUNSETPERSIST` ioctl setting/clearing `tun->flags & IFF_PERSIST`.
    pub fn set_persist(&self, persist: bool) {
        self.persist.store(persist, Ordering::Release);
    }

    /// Applies a new administrative MTU to the data plane (`SIOCSIFMTU`).
    ///
    /// Stores the *frame* ceiling (`mtu + l2_overhead`) both queues enforce, so
    /// after lowering the MTU an oversize datagram is dropped at the queue
    /// boundary rather than injected into or emitted from the stack. `mtu` is
    /// already range-checked by the ioctl handler against the device-kind bounds
    /// (68..=65535 for TUN, 68..=65521 for TAP), so `mtu + l2_overhead` never
    /// exceeds `TUN_FRAME_CAPACITY` and the allocation guard in `TunPacket::new`
    /// stays a strict upper bound.
    pub fn set_mtu(&self, mtu: usize) {
        self.frame_limit
            .store(mtu + self.l2_overhead, Ordering::Release);
    }

    /// Current per-interface frame-length ceiling (MTU + L2 overhead).
    fn frame_limit(&self) -> usize {
        self.frame_limit.load(Ordering::Acquire)
    }

    /// Queues a userspace-provided IP packet for the stack and wakes the router
    /// RX worker so it invokes [`TunDevice::recv`]. Returns `false` when the
    /// packet exceeds the MTU.
    pub fn push_rx(&self, packet: &[u8]) -> bool {
        // Drop a datagram larger than the configured MTU (frame ceiling =
        // MTU + L2 overhead) before it reaches the stack. This is a silent drop,
        // not an error: `TunFile::write_at` reports the full user length as
        // consumed regardless (matching Linux `tun_get_user`, which never fails a
        // write on an over-MTU frame), so userspace sees write()==len while the
        // packet never enters the RX queue.
        if packet.len() > self.frame_limit() {
            return false;
        }
        if !self.rx_queue.push(packet) {
            return false;
        }
        // The RX worker's waker is registered on this poll set with `IN`.
        // Waking here is what pulls the packet into the stack.
        // SAFETY: `PollSet::wake` must run with preemption and IRQs enabled
        // (its waker path may re-enter poll registration); `push_rx` is called
        // from the userspace TUN write/ioctl path, never from an IRQ or
        // preempt-disabled context, so the precondition holds.
        unsafe { self.poll_set.wake(IoEvents::IN) };
        true
    }

    /// Pops one stack-produced packet destined for userspace `read(2)`.
    pub fn pop_tx(&self) -> Option<Vec<u8>> {
        self.tx_queue.pop().map(|packet| packet.as_slice().to_vec())
    }

    /// Whether a `read(2)` would return a packet without blocking.
    pub fn has_tx(&self) -> bool {
        !self.tx_queue.is_empty()
    }

    /// Poll set used by the char device to arm `read`/`poll` waiters.
    pub fn poll_set(&self) -> &Arc<PollSet> {
        &self.poll_set
    }

    /// Builds a bare, unregistered [`TunShared`] for kernel axtest coverage of
    /// the `/dev/net/tun` attach/rollback state machine.
    ///
    /// This bypasses `Service::create_tun`, so the returned handle is not filed
    /// in the router registry and never carries traffic. That is exactly what
    /// the rollback test needs: it drives `try_attach()`/`detach()`/`mark_dying`
    /// on the [`AttachState`] slot in isolation, without a running net worker.
    #[cfg(axtest)]
    pub fn new_detached_for_test(name: String) -> Arc<Self> {
        Self::new(name, 0)
    }

    /// Reports whether the single-queue slot is currently claimed.
    ///
    /// Test-only observer of the `Attached` state, letting a kernel axtest
    /// assert the exact post-rollback state machine position rather than
    /// inferring it from a follow-up `try_attach()` (which would itself mutate
    /// the slot). `Free` and `Dying` are both "not attached"; the two are told
    /// apart by [`Self::is_dying_for_test`].
    #[cfg(axtest)]
    pub fn is_attached_for_test(&self) -> bool {
        *self.attach_state.lock_irqsave() == AttachState::Attached
    }

    /// Reports whether the device has entered the terminal `Dying` state.
    ///
    /// Test-only observer used to distinguish a created device that was fully
    /// torn down (`mark_dying` + `detach`) from a pre-existing one that was only
    /// released back to `Free`.
    #[cfg(axtest)]
    pub fn is_dying_for_test(&self) -> bool {
        *self.attach_state.lock_irqsave() == AttachState::Dying
    }
}

/// Router-side half of a TUN interface.
///
/// Holds a clone of the [`TunShared`] so both halves see the same queues and
/// poll set. The device's IPv4 configuration is tracked only so route helpers
/// have somewhere to publish it; TUN performs no source-address logic itself.
pub struct TunDevice {
    shared: Arc<TunShared>,
    ip: Option<Ipv4Cidr>,
}

impl TunDevice {
    /// Creates a TUN device and returns the router half plus the shared handle
    /// the char device must retain.
    pub fn new(name: String) -> (Self, Arc<TunShared>) {
        let shared = TunShared::new(name, 0);
        (
            Self {
                shared: shared.clone(),
                ip: None,
            },
            shared,
        )
    }
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
        // One packet per call keeps parity with the ethernet adapter, whose
        // worker loops until the buffer is full or `recv` returns 0.
        let Some(packet) = self.shared.rx_queue.pop() else {
            return 0;
        };
        let payload = packet.as_slice();
        // A user write larger than the smoltcp RX slot is impossible: the queue
        // already rejected anything above the MTU, and the router RX buffer is
        // sized to the MTU. `enqueue` therefore only fails on a genuinely full
        // buffer, in which case dropping matches a saturated hardware ring.
        match buffer.enqueue(payload.len(), interface_id) {
            Ok(dst) => {
                snoop(payload);
                dst.copy_from_slice(payload);
                // TUN is IP-only: no Ethernet header. The L2 frame length
                // aligns with Linux /proc/net/dev rx_bytes counting for tun
                // interfaces, which counts the raw IP packet bytes.
                payload.len()
            }
            Err(_) => 0,
        }
    }

    fn send(&mut self, _next_hop: IpAddress, packet: &[u8], _timestamp: Instant) -> usize {
        // The shared smoltcp wire is pinned at STACK_MAX_MTU and never lowered
        // (router capabilities are cached once at Interface::new), so the core
        // can hand us a frame larger than a *lowered* interface MTU. Drop such a
        // frame here rather than emitting it to read(2): this is the data-plane
        // equivalent of Linux's IP layer refusing to xmit an over-MTU skb on the
        // netdev, mirroring `tun_net_xmit`'s discipline of dropping
        // (NET_XMIT_DROP) with no errno.
        if packet.len() > self.shared.frame_limit() {
            return 0;
        }
        // Deliver the routed IP packet to userspace. `send` runs while the
        // router holds this device's lock, so the reader wakeup is deferred to
        // the net worker (drained outside all locks) rather than run inline,
        // per the poll-set safety contract.
        if self.shared.tx_queue.push(packet) {
            crate::defer_poll_wake(self.shared.poll_set.clone(), IoEvents::IN);
            // TUN is IP-only: return the IP packet length as the L2 frame
            // length, matching Linux /proc/net/dev tx_bytes for tun interfaces.
            packet.len()
        } else {
            0
        }
    }

    fn set_ipv4_addr(&mut self, addr: Option<Ipv4Cidr>) {
        self.ip = addr;
    }

    fn readiness_poll(&self) -> Option<Arc<PollSet>> {
        // The router RX worker arms its waker here; user writes wake it.
        Some(self.shared.poll_set.clone())
    }
}

/// Builds a layer-2 TAP device plus the shared handle the char fd retains.
///
/// A TAP presents an Ethernet medium: userspace exchanges whole frames and the
/// stack must ARP-resolve next hops and frame every outbound IP packet. Instead
/// of reimplementing that, a TAP is an [`EthernetDevice`] whose driver
/// ([`TunEthernetDriver`]) is nothing but the two [`TunShared`] queues dressed
/// as a NIC. The device adopts the shared poll set as its readiness signal, so
/// the exact same `write(2)`/`read(2)` wake paths a TUN uses drive the layer-2
/// device: `write(2)` wakes the RX worker, stack-produced frames wake `read(2)`.
pub fn create_tap(name: String, mac: [u8; 6]) -> (EthernetDevice, Arc<TunShared>) {
    let shared = TunShared::new(name.clone(), ETHERNET_HEADER_LEN);
    let driver = TunEthernetDriver::new(shared.clone(), mac);
    let mut device = EthernetDevice::new_oob_rx_with_poll_set(
        name,
        Box::new(driver),
        None,
        shared.poll_set.clone(),
    );
    // A TAP is implicitly promiscuous: Linux tun_get_user injects frames via
    // netif_rx without filtering by destination MAC (tun.c:2045), so frames
    // addressed to any peer MAC reach the stack unfiltered.
    device.set_promiscuous(true);
    (device, shared)
}

/// Ethernet driver that turns a pair of [`TunShared`] queues into a virtual NIC.
///
/// There is no hardware and no interrupt: `transmit` hands a finished frame to
/// userspace `read(2)`, `receive` takes the next frame userspace wrote, and RX
/// readiness is delivered out-of-band through the shared poll set. This lets a
/// TAP reuse [`EthernetDevice`]'s ARP, neighbor, and framing machinery verbatim.
struct TunEthernetDriver {
    shared: Arc<TunShared>,
    mac: [u8; 6],
}

impl TunEthernetDriver {
    fn new(shared: Arc<TunShared>, mac: [u8; 6]) -> Self {
        Self { shared, mac }
    }
}

impl EthernetDriver for TunEthernetDriver {
    fn device_name(&self) -> &str {
        self.shared.name()
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
        Ok(Box::new(TunTxBuffer::new(size)))
    }

    fn recycle_tx_buffers(&mut self) -> NetDeviceResult {
        Ok(())
    }

    fn transmit(&mut self, tx_buf: &mut dyn NetTxBuffer) -> NetDeviceResult {
        // Push the finished Ethernet frame toward userspace `read(2)` and wake a
        // blocked reader. Unlike [`TunDevice::send`], which runs under the
        // router's sleeping device mutex and can defer the wake, `transmit` is
        // called from [`EthernetDevice`] while its driver `SpinLock` is held
        // (ARP replies are emitted inline during `recv`), so IRQs and preemption
        // are disabled here. The deferred-wake path takes a sleeping mutex and
        // the plain `PollSet::wake` allocates - both are illegal in this
        // context. `wake_from_irq` is the allocation-free, IRQ-safe wake the
        // hardware ethernet IRQ path uses, and it is correct here for the same
        // reason: it only drains preinitialized waiter slots and marks tasks
        // runnable without sleeping.
        // Drop an Ethernet frame exceeding the interface's frame ceiling
        // (MTU + ETH_HLEN) before it reaches read(2), for the same reason as
        // `TunDevice::send`: the shared wire is over-provisioned at STACK_MAX_MTU,
        // so a lowered TAP MTU is only honored if the device boundary enforces
        // it. Silent drop, no errno (tun_net_xmit / NET_XMIT_DROP).
        if tx_buf.packet().len() <= self.shared.frame_limit()
            && self.shared.tx_queue.push(tx_buf.packet())
        {
            self.shared.poll_set.wake_from_irq(IoEvents::IN);
        }
        Ok(())
    }

    fn receive(&mut self) -> NetDeviceResult<Box<dyn NetRxBuffer>> {
        match self.shared.rx_queue.pop() {
            Some(packet) => Ok(Box::new(TunRxBuffer::new(packet.as_slice().to_vec()))),
            None => Err(NetDeviceError::Again),
        }
    }

    fn recycle_rx_buffer(&mut self, _rx_buf: &mut dyn NetRxBuffer) -> NetDeviceResult {
        Ok(())
    }

    fn handle_irq(&mut self) -> NetIrqEvents {
        NetIrqEvents::empty()
    }
}

/// Heap-backed TX buffer handed to [`EthernetDevice`] for one outbound frame.
struct TunTxBuffer {
    packet: Vec<u8>,
}

impl TunTxBuffer {
    fn new(size: usize) -> Self {
        Self {
            packet: alloc::vec![0u8; size],
        }
    }
}

impl NetTxBuffer for TunTxBuffer {
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

/// Heap-backed RX buffer carrying one frame userspace wrote to the char fd.
struct TunRxBuffer {
    packet: Vec<u8>,
}

impl TunRxBuffer {
    fn new(packet: Vec<u8>) -> Self {
        Self { packet }
    }
}

impl NetRxBuffer for TunRxBuffer {
    fn packet(&self) -> &[u8] {
        &self.packet
    }
}
