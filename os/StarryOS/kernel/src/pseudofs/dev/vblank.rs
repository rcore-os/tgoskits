//! Synthesized 60 Hz vblank clock for the emulated `/dev/dri/card0`.
//!
//! The card has no real scanout engine — presentation is a synchronous
//! memcpy — so there is no hardware counter to latch vblank edges from.
//! Linux DRM userspace (libdrm, compositors) nevertheless expects a
//! monotonic per-CRTC sequence advancing at the mode's refresh rate,
//! plus timestamps of the most recent edge (`CRTC_GET_SEQUENCE`,
//! `CRTC_QUEUE_SEQUENCE`, `WAIT_VBLANK`, and flip-completion events).
//! The clock here derives that sequence from elapsed monotonic time
//! anchored at card creation, mirroring Linux's
//! `vblank_disable_immediate` mode where the counter is computed from
//! timestamps rather than latched by an interrupt
//! (`drivers/gpu/drm/drm_vblank.c`, `drm_vblank_count_and_time`).
//!
//! Queued events (`CRTC_QUEUE_SEQUENCE`, `WAIT_VBLANK` with
//! `_DRM_VBLANK_EVENT`) are delivered lazily by [`Card0::poll`], not by
//! a kernel timer thread: real hardware raises a vblank IRQ per edge,
//! and the emulation's equivalent observation point is userspace
//! polling the DRM fd. Delivery latency is therefore bounded by the
//! caller's poll interval rather than the vblank period, while event
//! timestamps always carry the synthesized edge time.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::sync::Mutex;

/// Nanoseconds between synthesized vblank edges (60 Hz, matching the
/// mode's `DEFAULT_VREFRESH` advertised by the card).
pub const VBLANK_PERIOD_NS: u64 = 1_000_000_000 / 60;

/// Upper bound on simultaneously pending vblank events, matching the
/// event-queue cap on the card.
pub const MAX_PENDING_EVENTS: usize = 128;

/// Wrap-aware "has the counter reached `target`" test on u64 sequences.
/// Mirrors Linux's `vblank_passed()` in `drivers/gpu/drm/drm_vblank.c`:
/// the comparison survives counter wraparound by interpreting the
/// difference as signed. The u32 userspace view is covered by casting
/// truncated values back to u64, which preserves the wrap semantics the
/// ABI hands out.
pub const fn vblank_passed(current: u64, target: u64) -> bool {
    current == target || (current.wrapping_sub(target) as i64) > 0
}

/// Linux's `widen_32_to_64()` (`drm_vblank.c`): reconstructs the full
/// u64 sequence a u32 counter value refers to, given a nearby full-width
/// reference. Low values just above a wrap resolve to the next cycle;
/// values that look "behind" the reference's low half resolve to the
/// reference's own high word.
pub const fn widen_32_to_64(low: u32, reference: u64) -> u64 {
    let high_span = 0xffff_ffff_0000_0000u64;
    let sign_fix = if low & 0x8000_0000 != 0 {
        0
    } else {
        high_span
    };
    (low as u64).wrapping_add(reference & high_span ^ sign_fix)
}

/// An event queued for a future vblank edge by `CRTC_QUEUE_SEQUENCE` or
/// the `_DRM_VBLANK_EVENT` variant of `WAIT_VBLANK`.
#[derive(Debug)]
pub(super) enum QueuedVblankEvent {
    /// `DRM_EVENT_VBLANK` — `WAIT_VBLANK` with `_DRM_VBLANK_EVENT`.
    Vblank { user_data: u64 },
    /// `DRM_EVENT_CRTC_SEQUENCE` — `CRTC_QUEUE_SEQUENCE`.
    CrtcSequence { user_data: u64 },
}

#[derive(Debug)]
pub(super) struct PendingVblankEvent {
    pub event: QueuedVblankEvent,
    /// Full-width sequence the event fires at.
    pub target_sequence: u64,
}

/// Time-derived vblank sequence source plus the queue of events waiting
/// for future edges. Shared state is a plain mutex; both writers (ioctl
/// tasks queuing) and the drainer (`Card0::poll`) are sleepable task
/// contexts, and no lock is held across another acquisition.
pub(super) struct VblankScheduler {
    clock: VblankClock,
    pending: Mutex<Vec<PendingVblankEvent>>,
}

impl VblankScheduler {
    pub(super) fn new(now_ns: u64) -> Self {
        Self {
            clock: VblankClock::new(now_ns),
            pending: Mutex::new(Vec::new()),
        }
    }

    pub(super) fn clock(&self) -> &VblankClock {
        &self.clock
    }

    /// Queues an event for a future edge. Returns `false` when the
    /// pending cap is reached (the caller maps this to Linux's
    /// event-reservation `-ENOMEM`).
    pub(super) fn queue(&self, event: PendingVblankEvent) -> bool {
        let mut pending = self.pending.lock();
        if pending.len() >= MAX_PENDING_EVENTS {
            return false;
        }
        pending.push(event);
        true
    }

    /// Removes and returns every event whose target edge has passed at
    /// `now_ns`. Called from `Card0::poll`.
    pub(super) fn take_expired(&self, now_ns: u64) -> Vec<PendingVblankEvent> {
        let current = self.clock.sequence_at(now_ns);
        let mut pending = self.pending.lock();
        let (expired, remaining) = pending
            .drain(..)
            .partition(|event| vblank_passed(current, event.target_sequence));
        *pending = remaining;
        expired
    }
}

/// Sequence counter derived from elapsed monotonic time. Sequence 0 is
/// the card-creation anchor; edge *N* occurs at
/// `anchor_ns + N * VBLANK_PERIOD_NS`.
pub(super) struct VblankClock {
    anchor_ns: AtomicU64,
}

impl VblankClock {
    fn new(now_ns: u64) -> Self {
        Self {
            anchor_ns: AtomicU64::new(now_ns),
        }
    }

    /// The most recent completed edge's sequence number at `now_ns`.
    pub(super) fn sequence_at(&self, now_ns: u64) -> u64 {
        let anchor = self.anchor_ns.load(Ordering::Relaxed);
        now_ns.saturating_sub(anchor) / VBLANK_PERIOD_NS
    }

    /// Monotonic timestamp (nanoseconds) of edge `sequence`. Saturates
    /// instead of overflowing for far-future targets.
    pub(super) fn edge_ns_of(&self, sequence: u64) -> u64 {
        let anchor = self.anchor_ns.load(Ordering::Relaxed);
        anchor.saturating_add(sequence.saturating_mul(VBLANK_PERIOD_NS))
    }
}

#[cfg(all(test, not(axtest)))]
mod tests {
    use super::*;

    #[test]
    fn vblank_passed_tracks_order_and_wrap() {
        // Equal counts as passed (the event fires on its own edge).
        assert!(vblank_passed(5, 5));
        assert!(!vblank_passed(4, 5));
        assert!(vblank_passed(6, 5));
        // Wrap boundary: a counter that wrapped to 0 has passed a target
        // just below u64::MAX.
        assert!(vblank_passed(u64::MAX, u64::MAX - 1));
        assert!(vblank_passed(0, u64::MAX));
        // Far-future targets must not look passed through the wrap.
        assert!(!vblank_passed(1, u64::MAX));
    }

    #[test]
    fn widen_resolves_low_values_near_reference() {
        // Plain small value near a small counter: unchanged.
        assert_eq!(widen_32_to_64(5, 0), 5);
        // Value in the same high-word neighborhood as the reference.
        assert_eq!(widen_32_to_64(5, 0x1_0000_0005), 0x1_0000_0005);
        // A "negative-looking" u32 near the wrap widens forward.
        assert_eq!(widen_32_to_64(0xffff_fff0, 0), 0xffff_fff0);
        // Reference with a nonzero high word pulls small lows up with it.
        assert_eq!(widen_32_to_64(0xffff_fff0, 0x2_0000_0000), 0x2_ffff_fff0);
    }

    #[test]
    fn clock_sequences_track_elapsed_periods() {
        let clock = VblankClock::new(1_000);
        assert_eq!(clock.sequence_at(1_000), 0);
        // Just before the first edge.
        assert_eq!(clock.sequence_at(1_000 + VBLANK_PERIOD_NS - 1), 0);
        // On the first edge.
        assert_eq!(clock.sequence_at(1_000 + VBLANK_PERIOD_NS), 1);
        // Three and a half periods later.
        assert_eq!(
            clock.sequence_at(1_000 + VBLANK_PERIOD_NS * 7 / 2),
            3
        );
        // Edge timestamps round-trip through the sequence computation.
        assert_eq!(clock.edge_ns_of(4), 1_000 + VBLANK_PERIOD_NS * 4);
    }
}
