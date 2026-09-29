//! Physical NIC layer-2 uplink bridge.
//!
//! The host stack owns exactly one physical NIC through the fixed-CPU queue
//! runtime, and a hypervisor cannot clone the driver, so a guest NIC is bridged
//! into that single owner instead:
//!
//! * **Egress (guest -> wire):** a guest device submits a complete Ethernet
//!   frame into a bounded, lock-free, non-blocking ring. The protocol executor
//!   drains that ring into the existing NIC TX path, so the frame reaches the
//!   hardware only through the queue owner that already holds the DMA tokens.
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
//! therefore never allocates, sleeps, or takes any lock. Shared state is a fixed
//! slot array plus atomics, and the ingress sink and bound device name are
//! published once through [`OnceLock`]. A full ring rejects the frame instead of
//! blocking or growing, so the same code runs in a bare-metal kernel and in a
//! pure host test.

use alloc::{boxed::Box, string::String, sync::Arc};
use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicU8, AtomicUsize, Ordering},
};

use ax_lazyinit::OnceLock;

use crate::device::ETHERNET_FRAME_CAPACITY;

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
    /// The bounded ring is full; the caller must drop or retry later.
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

/// One egress slot state. The ordering of these transitions is the whole
/// publication protocol: `EMPTY -> FILLING -> READY -> DRAINING -> EMPTY`.
const SLOT_EMPTY: u8 = 0;
const SLOT_FILLING: u8 = 1;
const SLOT_READY: u8 = 2;
const SLOT_DRAINING: u8 = 3;

struct EgressSlot {
    state: AtomicU8,
    frame_len: AtomicUsize,
    data: UnsafeCell<[u8; ETHERNET_FRAME_CAPACITY]>,
}

impl EgressSlot {
    /// Copies one frame into the slot payload.
    ///
    /// # Safety
    ///
    /// The caller must have claimed this slot with `EMPTY -> FILLING` and must
    /// not publish it as `READY` until this returns, so no other execution
    /// context can access `data` concurrently. `frame.len()` is checked against
    /// [`ETHERNET_FRAME_CAPACITY`] by the caller.
    unsafe fn write_frame(&self, frame: &[u8]) {
        debug_assert!(frame.len() <= ETHERNET_FRAME_CAPACITY);
        // SAFETY: `self.data` is an `UnsafeCell`, so a raw pointer is the only
        // way to reach the payload; the caller's `FILLING` ownership makes the
        // pointer the unique mutable borrow for `frame.len()` bytes, which the
        // caller bounded by the slot capacity.
        let destination =
            unsafe { core::slice::from_raw_parts_mut(self.data.get().cast::<u8>(), frame.len()) };
        destination.copy_from_slice(frame);
    }

    /// Returns the slot payload as a bounded shared slice.
    ///
    /// # Safety
    ///
    /// The caller must have claimed this slot with `READY -> DRAINING` and must
    /// keep it out of `EMPTY` for as long as the returned slice is used, so the
    /// producer cannot reuse the slot while it is being read. `frame_len` must
    /// be the length published with the frame and must not exceed
    /// [`ETHERNET_FRAME_CAPACITY`].
    unsafe fn frame(&self, frame_len: usize) -> &[u8] {
        debug_assert!(frame_len <= ETHERNET_FRAME_CAPACITY);
        // SAFETY: the `DRAINING` claim gives this single reader exclusive
        // access to the payload, and the producer only reuses the slot after
        // the state returns to `EMPTY`.
        unsafe { core::slice::from_raw_parts(self.data.get().cast::<u8>(), frame_len) }
    }
}

// SAFETY: the payload is written only by the producer that won the slot's
// `EMPTY -> FILLING` compare-and-swap and read only by the single drainer after
// it claims `READY -> DRAINING` with an acquire. Those states are mutually
// exclusive, and the `Release`/`Acquire` pair on `state` publishes the payload
// and `frame_len` before any read, so the module-private `unsafe` accessors are
// the only ways to reach the payload.
unsafe impl Sync for EgressSlot {}

/// Bounded multi-producer / single-consumer egress ring.
///
/// Producer registration is by monotonically increasing ticket, so concurrent
/// vCPUs of several guests never touch the same slot unless the ring is full;
/// a full ring rejects the frame instead of blocking.
struct EgressRing {
    slots: Box<[EgressSlot]>,
    ticket: AtomicUsize,
    /// Round-robin start for the single drainer: a drain pass bounded to fewer
    /// slots than the ring depth must not always restart at slot 0, or a busy
    /// producer that refills the low slots would starve the high ones.
    cursor: AtomicUsize,
}

impl EgressRing {
    fn new(depth: usize) -> Self {
        let depth = depth.max(1);
        let slots = (0..depth)
            .map(|_| EgressSlot {
                state: AtomicU8::new(SLOT_EMPTY),
                frame_len: AtomicUsize::new(0),
                data: UnsafeCell::new([0u8; ETHERNET_FRAME_CAPACITY]),
            })
            .collect::<alloc::vec::Vec<_>>()
            .into_boxed_slice();
        Self {
            slots,
            ticket: AtomicUsize::new(0),
            cursor: AtomicUsize::new(0),
        }
    }

    fn submit(&self, frame: &[u8]) -> Result<(), UplinkEgressError> {
        if frame.len() < 14 || frame.len() > ETHERNET_FRAME_CAPACITY {
            return Err(UplinkEgressError::InvalidFrame);
        }

        let ticket = self.ticket.fetch_add(1, Ordering::Relaxed);
        let slot = &self.slots[ticket % self.slots.len()];
        if slot
            .state
            .compare_exchange(
                SLOT_EMPTY,
                SLOT_FILLING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return Err(UplinkEgressError::Full);
        }

        // SAFETY: this producer holds the slot in `FILLING`, and `frame.len()`
        // was just checked against the slot capacity.
        unsafe { slot.write_frame(frame) };
        slot.frame_len.store(frame.len(), Ordering::Relaxed);
        slot.state.store(SLOT_READY, Ordering::Release);
        Ok(())
    }

    /// Drains up to `budget` ready frames.
    ///
    /// Must have exactly one concurrent caller: the protocol executor of the
    /// interface this uplink is bound to. `transmit` returns `true` once it has
    /// taken ownership of the frame; `false` leaves the slot ready for the next
    /// poll and stops this pass.
    ///
    /// The scan starts at a round-robin cursor that advances past each drained
    /// slot, so a budget smaller than the ring depth still serves every slot in
    /// turn. A frame a producer published behind the cursor is picked up by the
    /// next pass. Work per call stays bounded by `slots.len()`.
    fn drain(&self, budget: usize, transmit: &mut dyn FnMut(&[u8]) -> bool) -> usize {
        let depth = self.slots.len();
        let mut cursor = self.cursor.load(Ordering::Relaxed);
        let mut drained = 0;
        for _ in 0..depth {
            if drained >= budget {
                break;
            }
            let index = cursor % depth;
            let slot = &self.slots[index];
            if slot
                .state
                .compare_exchange(
                    SLOT_READY,
                    SLOT_DRAINING,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_err()
            {
                cursor = index + 1;
                continue;
            }
            let frame_len = slot.frame_len.load(Ordering::Relaxed);
            if frame_len == 0 || frame_len > ETHERNET_FRAME_CAPACITY {
                slot.state.store(SLOT_EMPTY, Ordering::Release);
                cursor = index + 1;
                continue;
            }
            // SAFETY: this drainer holds the slot in `DRAINING` and
            // `frame_len` was validated against the slot capacity above.
            let accepted = transmit(unsafe { slot.frame(frame_len) });
            if accepted {
                slot.state.store(SLOT_EMPTY, Ordering::Release);
                cursor = index + 1;
                drained += 1;
            } else {
                // Keep the cursor on the rejected slot: it still holds a frame
                // and must be retried first on the next pass.
                slot.state.store(SLOT_READY, Ordering::Release);
                break;
            }
        }
        self.cursor.store(cursor % depth, Ordering::Relaxed);
        drained
    }
}

/// Shared physical-uplink runtime for one host NIC.
///
/// Synchronization is runtime-agnostic on purpose: the hot paths do not take
/// any lock. The egress ring is atomic, and the ingress sink and bound device
/// name are published once with `OnceLock`, so this type is equally usable from
/// a bare-metal kernel and from a pure host test that never boots a kernel.
pub struct UplinkRuntime {
    egress: EgressRing,
    ingress: OnceLock<Arc<dyn IngressSink>>,
    /// Host interface this uplink owns; frames on any other interface are left
    /// to their own stack so a second NIC cannot steal guest traffic.
    owner_device: OnceLock<String>,
}

impl UplinkRuntime {
    /// Creates an uplink with a bounded egress ring of `depth` slots.
    fn new(depth: usize) -> Self {
        Self {
            egress: EgressRing::new(depth),
            ingress: OnceLock::new(),
            owner_device: OnceLock::new(),
        }
    }

    /// Binds the uplink to one host interface by name.
    ///
    /// Installed exactly once, before any guest device exists; later calls are
    /// ignored so the binding cannot silently move to another NIC.
    pub fn bind_device(&self, name: &str) {
        let _ = self.owner_device.call_once(|| name.into());
    }

    /// Returns whether `name` is the interface bound to this uplink.
    pub fn matches_device(&self, name: &str) -> bool {
        self.owner_device
            .get()
            .is_some_and(|bound| bound.as_str() == name)
    }

    /// Installs the switch-side ingress sink once during hypervisor startup.
    pub fn set_ingress_sink(&self, sink: Arc<dyn IngressSink>) {
        let _ = self.ingress.call_once(|| sink);
    }

    /// Submits one complete guest frame for transmission on the physical NIC.
    ///
    /// Bounded and non-blocking: safe in a vCPU MMIO-write path. A successful
    /// submission requests a protocol poll so the frame is drained promptly.
    pub fn submit_egress(&self, frame: &[u8]) -> Result<(), UplinkEgressError> {
        self.egress.submit(frame)?;
        crate::request_poll();
        Ok(())
    }

    /// Drains queued guest frames into the NIC TX path.
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

/// Installs the process-wide physical uplink, if it is not installed yet.
///
/// Opt-in by construction: no uplink runtime exists until the hypervisor glue
/// calls this, so a plain ArceOS application pays only one failed `OnceLock`
/// lookup per received frame.
pub fn install(depth: usize) -> Arc<UplinkRuntime> {
    UPLINK
        .call_once(|| Arc::new(UplinkRuntime::new(depth)))
        .clone()
}

/// Returns the installed uplink, if any.
pub fn runtime() -> Option<Arc<UplinkRuntime>> {
    UPLINK.get().cloned()
}

#[cfg(test)]
mod tests {
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

        // The two queued frames still reach the TX path, then free their slots.
        let mut seen = alloc::vec::Vec::new();
        assert_eq!(
            ring.drain(8, &mut |f| {
                seen.push(f[6]);
                true
            }),
            2
        );
        seen.sort_unstable();
        assert_eq!(seen, alloc::vec![1, 2]);
        ring.submit(&frame(4, 64)).unwrap();

        // A frame below the Ethernet header is rejected before it can consume a
        // ring slot.
        assert_eq!(ring.submit(&[0u8; 8]), Err(UplinkEgressError::InvalidFrame));
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
}
