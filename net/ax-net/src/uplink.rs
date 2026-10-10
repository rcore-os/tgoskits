//! Physical NIC layer-2 uplink bridge.
//!
//! Each [`UplinkRuntime`] attaches a guest fabric to one selected physical
//! interface through the fixed-CPU queue runtime. A hypervisor cannot clone the
//! driver, so guest traffic is handed to that existing owner:
//!
//! * **Egress (guest -> wire):** a guest device submits a complete Ethernet
//!   frame into a bounded, non-blocking ring whose two ends are serialized by
//!   single-shot gates. The protocol executor drains that ring into the
//!   existing NIC TX path, so the frame reaches the hardware only through the
//!   queue owner that already holds the DMA tokens.
//! * **Ingress (wire -> guest):** every received physical frame is copied to
//!   the registered ingress sink *before* the host stack applies its
//!   host-MAC-only filter, because the switch must see frames addressed to
//!   guest MACs that the host interface would otherwise drop.
//!
//! Both directions are independent of the hypervisor: `ax-net` only defines
//! the bounded hand-off boundary; the switch and MAC ownership stay in the
//! caller that registers the sink.
//!
//! [`UplinkRuntime::submit_egress`] may run in a vCPU MMIO-write context and
//! therefore never allocates, sleeps, or waits: it either wins a single
//! non-blocking gate or is rejected. The ring preallocates its frame slots once,
//! and the ingress sink and bound interface id are published once through
//! [`OnceLock`]. A full or busy ring rejects the frame instead of blocking or
//! growing, so the same code runs in a bare-metal kernel and in a pure host
//! test.

use alloc::sync::Arc;
use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicBool, Ordering},
};

use ax_lazyinit::OnceLock;
use ringbuf::{
    HeapCons, HeapProd, HeapRb,
    traits::{Consumer, Producer, Split},
};

use crate::{config::InterfaceId, device::ETHERNET_FRAME_CAPACITY};

/// Maximum frames a single drain pass moves to the NIC TX path.
///
/// Keeps one protocol poll bounded even while a guest floods a broadcast
/// stream, so the host stack keeps making progress on its own traffic.
pub const EGRESS_DRAIN_BUDGET: usize = 32;

/// Default number of egress slots allocated by [`install`].
///
/// Sized for a short burst of guest frames plus the host stack's own software
/// TX queue (64 frames), so a guest burst cannot starve host traffic.
pub const DEFAULT_EGRESS_DEPTH: usize = 64;

/// Result of one bounded egress submission.
#[derive(Debug, Clone, Copy, Eq, PartialEq, thiserror::Error)]
pub enum UplinkEgressError {
    /// The bounded ring is full, or another producer currently holds the
    /// producer gate; the caller must drop or retry later.
    #[error("physical uplink egress ring is full")]
    Full,
    /// The frame is shorter than an Ethernet header or larger than the port.
    #[error("invalid physical uplink frame length")]
    InvalidFrame,
}

/// Receives a copy of every physical frame before the host MAC filter.
///
/// Implemented by the hypervisor switch adapter. Called on the protocol
/// executor, so implementations may copy into guest bounded ingress queues but
/// must not block on guest progress.
pub trait IngressSink: Send + Sync {
    /// Delivers one complete Ethernet frame (including the header).
    fn deliver_physical_rx(&self, frame: &[u8]);
}

/// One queued egress frame with its payload stored inline.
///
/// The ring preallocates `depth` of these, so `submit` and `drain` only move
/// and copy fixed-size payloads and never allocate.
struct EgressFrame {
    len: usize,
    data: [u8; ETHERNET_FRAME_CAPACITY],
}

impl EgressFrame {
    /// Creates an empty frame slot.
    const fn empty() -> Self {
        Self {
            len: 0,
            data: [0; ETHERNET_FRAME_CAPACITY],
        }
    }

    /// Returns the initialized prefix that holds the frame.
    fn payload(&self) -> &[u8] {
        &self.data[..self.len]
    }
}

/// RAII single-shot gate for one ring half.
///
/// Acquisition is one non-blocking compare-exchange, so a held gate rejects the
/// caller instead of spinning or waiting. Only the successful compare-exchange
/// creates a guard; a rejected acquisition constructs nothing and writes
/// nothing, so it cannot disturb the current holder. `Drop` releases the gate,
/// which also covers unwinding out of the guarded section.
struct EgressGate<'a> {
    gate: &'a AtomicBool,
}

impl<'a> EgressGate<'a> {
    fn try_acquire(gate: &'a AtomicBool) -> Option<Self> {
        match gate.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed) {
            Ok(_) => Some(Self { gate }),
            Err(_) => None,
        }
    }
}

impl Drop for EgressGate<'_> {
    fn drop(&mut self) {
        self.gate.store(false, Ordering::Release);
    }
}

/// Bounded multi-producer / single-consumer egress ring.
///
/// `ringbuf`'s heap SPSC buffer owns the preallocated frame slots, so neither
/// `submit` nor `drain` allocates. Each half is wrapped in an `UnsafeCell` and
/// guarded by its own single-shot gate:
///
/// * A producer must win the producer gate to reach the write half. Only one
///   producer runs there at a time, so a vCPU that finds the gate held is
///   rejected as [`UplinkEgressError::Full`] instead of waiting.
/// * A consumer must win the consumer gate to reach the read half, so a
///   concurrent or recursive `drain` returns `0` without touching the ring.
///
/// The two gates are independent and `drain` never takes the producer gate, so
/// a transmit callback may call `submit`, which only competes with other
/// producers for the producer gate.
struct EgressRing {
    producer_gate: AtomicBool,
    producer: UnsafeCell<HeapProd<EgressFrame>>,
    consumer_gate: AtomicBool,
    consumer: UnsafeCell<HeapCons<EgressFrame>>,
}

// SAFETY: `EgressRing` is shared between vCPU producer contexts and the single
// protocol-executor consumer, and `UplinkRuntime` wraps it in an `Arc`, so it
// must be `Sync`.
//
// Two different things are shared through this type:
// * The ring *ends* (`HeapProd`/`HeapCons`) hold non-`Sync` cached index state
//   in `Cell`s. Each end lives in its own `UnsafeCell` and is touched only by
//   the context that won that end's single-shot `AtomicBool` gate: `submit`
//   creates the `&mut HeapProd` only after winning `producer_gate`, and `drain`
//   creates the `&mut HeapCons` only after winning `consumer_gate`. The RAII
//   guard releases the gate only after that `&mut` is dead, and a recursive
//   `drain` fails the consumer gate before it can touch the end, so no two live
//   `&mut`s to one end can exist. The acquire/release pair on each gate orders
//   the previous holder's cached indices for the next holder.
// * The frame *payload slots* are one shared ring buffer, not two disjoint
//   allocations. Their ownership is what `ringbuf`'s SPSC contract provides: a
//   slot becomes exclusively writable only after the consumer reclaimed it
//   (published by the read-index update of `try_pop`) and exclusively readable
//   only after the producer published it (the write-index update of
//   `try_push`), both with release/acquire ordering. The gates above turn
//   several producers plus one consumer into a valid SPSC pair, so at most one
//   live `&mut EgressFrame` or `&EgressFrame` exists per slot.
// Every `&mut` stays inside its guarded section, so no `&`/`&mut` can alias it.
unsafe impl Sync for EgressRing {}

impl EgressRing {
    fn new(depth: usize) -> Self {
        let (producer, consumer) = HeapRb::<EgressFrame>::new(depth.max(1)).split();
        Self {
            producer_gate: AtomicBool::new(false),
            producer: UnsafeCell::new(producer),
            consumer_gate: AtomicBool::new(false),
            consumer: UnsafeCell::new(consumer),
        }
    }

    fn submit(&self, frame: &[u8]) -> Result<(), UplinkEgressError> {
        if frame.len() < 14 || frame.len() > ETHERNET_FRAME_CAPACITY {
            return Err(UplinkEgressError::InvalidFrame);
        }

        let Some(_producer_gate) = EgressGate::try_acquire(&self.producer_gate) else {
            // Another producer is already inside the ring. Reject instead of
            // waiting, because `submit` runs in a vCPU MMIO-write path.
            return Err(UplinkEgressError::Full);
        };
        // SAFETY: `_producer_gate` is the only producer-side gate for this
        // ring, so no other execution context can create a reference into
        // `producer` while this `&mut` is alive; the gate is released only
        // after the reference is dead.
        let producer = unsafe { &mut *self.producer.get() };
        let mut item = EgressFrame::empty();
        item.len = frame.len();
        item.data[..frame.len()].copy_from_slice(frame);
        producer.try_push(item).map_err(|_| UplinkEgressError::Full)
    }

    /// Drains up to `budget` frames, oldest first.
    ///
    /// Must have exactly one concurrent caller: the protocol executor of the
    /// interface this uplink is bound to. `transmit` returns `true` once it has
    /// taken ownership of the frame; `false` leaves the frame at the head of
    /// the ring and stops this pass, so the next poll retries the oldest frame
    /// first. Work per call stays bounded by `budget`.
    fn drain(&self, budget: usize, transmit: &mut dyn FnMut(&[u8]) -> bool) -> usize {
        let Some(_consumer_gate) = EgressGate::try_acquire(&self.consumer_gate) else {
            // A concurrent or recursive drain already owns the read side.
            return 0;
        };
        // SAFETY: `_consumer_gate` is the only consumer-side gate, so no other
        // execution context can create a second `&mut` into `consumer`; a
        // recursive `drain` fails the gate above before it could do so.
        let consumer = unsafe { &mut *self.consumer.get() };

        let mut drained = 0;
        while drained < budget {
            // Peek rather than pop, so a rejected transmit leaves the oldest
            // frame in place for the next pass. `ringbuf` stops a producer from
            // writing an occupied slot, so this borrow stays valid even if
            // `transmit` re-enters `submit`.
            let accepted = match consumer.first() {
                Some(frame) => transmit(frame.payload()),
                None => break,
            };
            if !accepted {
                break;
            }
            // The `first` borrow ended with the `match` above.
            if consumer.try_pop().is_none() {
                break;
            }
            drained += 1;
        }
        drained
    }
}

/// Shared physical-uplink runtime for one host network interface.
///
/// Synchronization is runtime-agnostic on purpose: the hot paths take no lock
/// and never wait. The egress ring uses one non-blocking gate per half, and the
/// ingress sink and bound interface id are published once with `OnceLock`, so
/// this type is equally usable from a bare-metal kernel and from a pure host
/// test that never boots a kernel.
pub struct UplinkRuntime {
    egress: EgressRing,
    ingress: OnceLock<Arc<dyn IngressSink>>,
    /// Stable host interface this uplink owns; frames on any other interface
    /// are left to their own stack so a second NIC cannot steal guest traffic.
    owner_interface: OnceLock<InterfaceId>,
}

impl UplinkRuntime {
    /// Creates an uplink with a bounded egress ring of `depth` slots.
    pub fn new(depth: usize) -> Self {
        Self {
            egress: EgressRing::new(depth),
            ingress: OnceLock::new(),
            owner_interface: OnceLock::new(),
        }
    }

    /// Binds the uplink to one host interface by stable identifier.
    ///
    /// Installed exactly once, before any guest device exists; later calls are
    /// ignored so the binding cannot silently move to another NIC.
    pub fn bind_interface(&self, id: InterfaceId) {
        let _ = self.owner_interface.call_once(|| id);
    }

    /// Returns whether `id` is the interface bound to this uplink.
    pub fn matches_interface(&self, id: InterfaceId) -> bool {
        self.owner_interface.get().is_some_and(|bound| *bound == id)
    }

    /// Installs the switch-side ingress sink once during hypervisor startup.
    pub fn set_ingress_sink(&self, sink: Arc<dyn IngressSink>) {
        let _ = self.ingress.call_once(|| sink);
    }

    /// Submits one complete guest frame for transmission on the physical device.
    ///
    /// Bounded and non-blocking: safe in a vCPU MMIO-write path. A successful
    /// submission requests a protocol poll so the frame is drained promptly; a
    /// full or busy ring returns [`UplinkEgressError::Full`] without waiting.
    pub fn submit_egress(&self, frame: &[u8]) -> Result<(), UplinkEgressError> {
        self.egress.submit(frame)?;
        crate::request_poll();
        Ok(())
    }

    /// Drains queued guest frames into the physical device TX path.
    ///
    /// Must run on the single protocol executor that owns the physical port;
    /// `transmit` reports `false` for transient backpressure.
    pub fn drain_egress(&self, budget: usize, transmit: &mut dyn FnMut(&[u8]) -> bool) -> usize {
        self.egress.drain(budget, transmit)
    }

    /// Delivers one received physical frame to the registered ingress sink.
    ///
    /// Called by the Ethernet device for every frame before the host MAC
    /// filter. Frames shorter than the Ethernet header are dropped here so the
    /// sink can assume a parseable header. Lock-free: one atomic `OnceLock`
    /// load keeps this usable on the protocol executor and in host tests.
    pub fn deliver_ingress(&self, frame: &[u8]) {
        if frame.len() < 14 {
            return;
        }
        if let Some(sink) = self.ingress.get() {
            sink.deliver_physical_rx(frame);
        }
    }
}

static UPLINK: OnceLock<Arc<UplinkRuntime>> = OnceLock::new();

/// Publishes a prepared physical uplink runtime, if no runtime is installed.
///
/// The caller prepares the interface binding and ingress sink before publishing
/// this pointer. AxVisor then installs its hypervisor adapter first, so a
/// losing initializer never leaves a globally visible half-configured runtime.
pub fn install(runtime: Arc<UplinkRuntime>) -> bool {
    let candidate = Arc::clone(&runtime);
    let selected = UPLINK.call_once(|| runtime);
    Arc::ptr_eq(selected, &candidate)
}

/// Returns the installed uplink, if any.
pub fn runtime() -> Option<Arc<UplinkRuntime>> {
    UPLINK.get().cloned()
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::AtomicUsize;

    use super::*;

    fn frame(src_tag: u8, len: usize) -> alloc::vec::Vec<u8> {
        let mut f = alloc::vec![0u8; len.max(14)];
        f[6] = src_tag;
        f
    }

    #[test]
    fn egress_ring_is_bounded_and_rejects_when_full() {
        let ring = EgressRing::new(2);
        ring.submit(&frame(1, 64)).unwrap();
        ring.submit(&frame(2, 64)).unwrap();
        assert_eq!(ring.submit(&frame(3, 64)), Err(UplinkEgressError::Full));

        // The two queued frames still reach the TX path in submission order,
        // then free their slots.
        let mut seen = alloc::vec::Vec::new();
        assert_eq!(
            ring.drain(8, &mut |f| {
                seen.push(f[6]);
                true
            }),
            2
        );
        assert_eq!(seen, alloc::vec![1, 2]);
        ring.submit(&frame(4, 64)).unwrap();

        // A frame below the Ethernet header is rejected before it can consume a
        // ring slot.
        assert_eq!(ring.submit(&[0u8; 8]), Err(UplinkEgressError::InvalidFrame));
    }

    #[test]
    fn egress_ring_preserves_fifo_order_across_slot_reuse() {
        // Depth 2 makes buffer reuse observable. After A/B drain, the next two
        // submissions reuse the same storage; an implementation that does not
        // preserve submission order drains the second burst as E/D.
        let ring = EgressRing::new(2);
        ring.submit(&frame(0xa1, 64)).unwrap();
        ring.submit(&frame(0xb2, 64)).unwrap();
        assert_eq!(ring.submit(&frame(0xc3, 64)), Err(UplinkEgressError::Full));

        let mut drained = alloc::vec::Vec::new();
        assert_eq!(
            ring.drain(8, &mut |f| {
                drained.push(f[6]);
                true
            }),
            2
        );
        assert_eq!(drained, alloc::vec![0xa1, 0xb2]);

        ring.submit(&frame(0xd4, 64)).unwrap();
        ring.submit(&frame(0xe5, 64)).unwrap();

        let mut redrained = alloc::vec::Vec::new();
        assert_eq!(
            ring.drain(8, &mut |f| {
                redrained.push(f[6]);
                true
            }),
            2
        );
        assert_eq!(redrained, alloc::vec![0xd4, 0xe5]);
    }

    #[test]
    fn ingress_dispatch_and_guest_egress() {
        struct Sink(AtomicUsize);
        impl IngressSink for Sink {
            fn deliver_physical_rx(&self, _frame: &[u8]) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
        let runtime = UplinkRuntime::new(4);
        let sink = Arc::new(Sink(AtomicUsize::new(0)));
        runtime.set_ingress_sink(sink.clone());

        runtime.deliver_ingress(&frame(1, 64));
        // Undersize frames never reach the sink.
        runtime.deliver_ingress(&[0u8; 8]);
        assert_eq!(sink.0.load(Ordering::Relaxed), 1);

        // A guest egress frame drains into the physical TX path.
        runtime.submit_egress(&frame(2, 64)).unwrap();
        assert_eq!(runtime.drain_egress(8, &mut |_| true), 1);
    }

    #[test]
    fn binding_matches_the_stable_interface_id() {
        let runtime = UplinkRuntime::new(2);
        let selected = InterfaceId::new(37);
        runtime.bind_interface(selected);

        assert!(runtime.matches_interface(selected));
        assert!(!runtime.matches_interface(InterfaceId::new(38)));
    }

    #[test]
    fn drain_budget_bounds_one_pass_and_keeps_the_rest_queued() {
        let ring = EgressRing::new(4);
        for tag in 1..=3u8 {
            ring.submit(&frame(tag, 64)).unwrap();
        }

        let mut first = alloc::vec::Vec::new();
        assert_eq!(
            ring.drain(2, &mut |f| {
                first.push(f[6]);
                true
            }),
            2
        );
        assert_eq!(first, alloc::vec![1, 2]);

        // The remaining frame is still queued and drains in order next pass.
        let mut rest = alloc::vec::Vec::new();
        assert_eq!(
            ring.drain(2, &mut |f| {
                rest.push(f[6]);
                true
            }),
            1
        );
        assert_eq!(rest, alloc::vec![3]);
    }

    #[test]
    fn rejected_transmit_keeps_the_oldest_frame() {
        let ring = EgressRing::new(4);
        ring.submit(&frame(0x41, 64)).unwrap();
        ring.submit(&frame(0x42, 64)).unwrap();

        // Backpressure refuses the head frame, so nothing is consumed and the
        // pass stops at once.
        let mut attempts = alloc::vec::Vec::new();
        assert_eq!(
            ring.drain(4, &mut |f| {
                attempts.push(f[6]);
                false
            }),
            0
        );
        assert_eq!(attempts, alloc::vec![0x41]);

        // A later pass retries from the same oldest frame.
        let mut seen = alloc::vec::Vec::new();
        assert_eq!(
            ring.drain(4, &mut |f| {
                seen.push(f[6]);
                true
            }),
            2
        );
        assert_eq!(seen, alloc::vec![0x41, 0x42]);
    }

    #[test]
    fn busy_producer_gate_rejects_repeatedly_without_clearing_the_holder() {
        let ring = EgressRing::new(4);
        // Model a vCPU already inside `submit`: hold the real producer gate and
        // check that two consecutive submissions are rejected rather than
        // blocking, and that neither rejected attempt releases the holder's
        // gate. This test holds no `&mut` into the ring.
        let held = EgressGate::try_acquire(&ring.producer_gate).expect("gate starts free");
        assert_eq!(ring.submit(&frame(0x51, 64)), Err(UplinkEgressError::Full));
        assert!(
            ring.producer_gate.load(Ordering::Acquire),
            "a rejected submit must not clear the holder's gate"
        );
        assert_eq!(ring.submit(&frame(0x52, 64)), Err(UplinkEgressError::Full));
        assert!(
            ring.producer_gate.load(Ordering::Acquire),
            "a second rejected submit must not clear the holder's gate"
        );

        // Only the holder releases the gate, and then a submission succeeds.
        drop(held);
        assert!(!ring.producer_gate.load(Ordering::Acquire));
        ring.submit(&frame(0x53, 64)).unwrap();
        let mut seen = alloc::vec::Vec::new();
        assert_eq!(
            ring.drain(4, &mut |f| {
                seen.push(f[6]);
                true
            }),
            1
        );
        assert_eq!(seen, alloc::vec![0x53]);
    }

    #[test]
    fn concurrent_drain_is_rejected_without_touching_the_ring() {
        let ring = EgressRing::new(4);
        ring.submit(&frame(0x61, 64)).unwrap();
        ring.submit(&frame(0x62, 64)).unwrap();

        let mut seen = alloc::vec::Vec::new();
        let mut recursive = alloc::vec::Vec::new();
        let drained = ring.drain(4, &mut |f| {
            seen.push(f[6]);
            // A recursive drain must fail the consumer gate and consume
            // nothing, leaving the outer pass in charge of the read side.
            let inner = ring.drain(4, &mut |g| {
                recursive.push(g[6]);
                true
            });
            assert_eq!(inner, 0);
            true
        });
        assert_eq!(drained, 2);
        assert_eq!(seen, alloc::vec![0x61, 0x62]);
        assert!(recursive.is_empty());
    }

    #[test]
    fn transmit_callback_may_reenter_submit() {
        let ring = EgressRing::new(4);
        ring.submit(&frame(0x71, 64)).unwrap();

        let mut pushed = false;
        let mut seen = alloc::vec::Vec::new();
        let drained = ring.drain(4, &mut |f| {
            seen.push(f[6]);
            if !pushed {
                pushed = true;
                // The consumer holds only the consumer gate, so a frame queued
                // from the callback is accepted and becomes the next head.
                ring.submit(&frame(0x72, 64)).expect("submit from callback");
            }
            true
        });
        assert!(pushed);
        assert_eq!(drained, 2);
        assert_eq!(seen, alloc::vec![0x71, 0x72]);
    }

    // Host-only: `catch_unwind` needs the standard library, which the ax-net
    // test build links.
    #[test]
    fn panicking_transmit_releases_the_consumer_gate() {
        let ring = EgressRing::new(4);
        ring.submit(&frame(0x81, 64)).unwrap();

        // A panic inside the transmit callback must release the consumer gate
        // through the guard's `Drop`, so the ring stays usable.
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            ring.drain(4, &mut |_| panic!("transmit callback panicked"));
        }))
        .is_err();
        assert!(panicked);

        // The frame was only peeked before the panic, so it is still the head
        // and a later pass drains it.
        let mut seen = alloc::vec::Vec::new();
        assert_eq!(
            ring.drain(4, &mut |f| {
                seen.push(f[6]);
                true
            }),
            1
        );
        assert_eq!(seen, alloc::vec![0x81]);
    }
}
