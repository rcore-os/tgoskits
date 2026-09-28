//! Synthesized 60 Hz vblank clock for the emulated `/dev/dri/card0`.
//!
//! The card has no real scanout engine — presentation is a synchronous
//! memcpy — so there is no hardware counter to latch vblank edges from.
//! Linux DRM userspace (libdrm, compositors) nevertheless expects a
//! monotonic per-CRTC sequence advancing at the mode's refresh rate,
//! plus timestamps of the most recent edge (`CRTC_GET_SEQUENCE`,
//! `CRTC_QUEUE_SEQUENCE`, `WAIT_VBLANK`, and flip-completion events).
//! The clock derives that sequence from elapsed monotonic time while
//! scanout is active. Disabling the CRTC freezes the counter; re-enabling
//! starts a new epoch without counting the disabled interval.
//!
//! Each open file owns its queued events and a deadline worker wakes readers
//! at the next edge; the clock and deadline-change notification are shared.

use crate::sync::RawSpinLock;

/// Nanoseconds between synthesized vblank edges (60 Hz, matching the
/// mode's `DEFAULT_VREFRESH` advertised by the card).
pub const VBLANK_PERIOD_NS: u64 = 1_000_000_000 / 60;

/// Wrap-aware "has the counter reached `target`" test on u64 sequences.
/// Linux's `drm_vblank_passed()` accepts only a bounded distance behind
/// the current sequence, so an old target outside that window stays pending.
pub const fn vblank_passed(current: u64, target: u64) -> bool {
    current.wrapping_sub(target) <= 1 << 23
}

/// Linux's `widen_32_to_64()` (`drm_vblank.c`): reconstructs the full
/// u64 sequence a u32 counter value refers to, given a nearby full-width
/// reference. Low values just above a wrap resolve to the next cycle;
/// values on either side of the reference resolve to the nearest wrap.
pub const fn widen_32_to_64(low: u32, reference: u64) -> u64 {
    reference.wrapping_add(low.wrapping_sub(reference as u32) as i32 as i64 as u64)
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

/// Sequence counter and the current active scanout epoch.
pub(super) struct VblankClock {
    state: RawSpinLock<VblankState>,
}

struct VblankState {
    active: bool,
    anchor_ns: u64,
    base_sequence: u64,
    last_edge_ns: u64,
    disable_generation: u64,
}

impl VblankState {
    fn at(&self, now_ns: u64) -> (u64, u64) {
        let elapsed = if self.active {
            now_ns.saturating_sub(self.anchor_ns) / VBLANK_PERIOD_NS
        } else {
            0
        };
        let edge_ns = if elapsed == 0 {
            self.last_edge_ns
        } else {
            self.anchor_ns
                .saturating_add(elapsed.saturating_mul(VBLANK_PERIOD_NS))
        };
        (self.base_sequence.saturating_add(elapsed), edge_ns)
    }

    fn edge_ns_of(&self, sequence: u64) -> u64 {
        if sequence <= self.base_sequence {
            self.last_edge_ns
        } else {
            self.anchor_ns.saturating_add(
                sequence
                    .saturating_sub(self.base_sequence)
                    .saturating_mul(VBLANK_PERIOD_NS),
            )
        }
    }
}

impl VblankClock {
    pub(super) fn new(now_ns: u64) -> Self {
        Self {
            state: RawSpinLock::new(VblankState {
                active: false,
                anchor_ns: now_ns,
                base_sequence: 0,
                last_edge_ns: now_ns,
                disable_generation: 0,
            }),
        }
    }

    /// Called under the device's modeset lock. Returns whether workers
    /// must recompute their deadlines after the transition.
    pub(super) fn set_active(&self, active: bool, now_ns: u64) -> bool {
        let mut state = self.state.lock();
        if state.active == active {
            return false;
        }
        if active {
            state.anchor_ns = now_ns;
            // A re-enable at sequence zero must not overwrite the disabled edge.
            if state.disable_generation == 0 {
                state.last_edge_ns = now_ns;
            }
        } else {
            (state.base_sequence, state.last_edge_ns) = state.at(now_ns);
            state.disable_generation = state.disable_generation.wrapping_add(1);
        }
        state.active = active;
        true
    }

    pub(super) fn active_at(&self, now_ns: u64) -> Option<(u64, u64)> {
        let state = self.state.lock();
        state.active.then(|| state.at(now_ns))
    }

    pub(super) fn snapshot_at(&self, now_ns: u64) -> (u64, u64) {
        self.state.lock().at(now_ns)
    }

    pub(super) fn disable_generation(&self) -> u64 {
        self.state.lock().disable_generation
    }

    /// A disable completes a wait even if the CRTC was re-enabled before
    /// the waiter ran. The frozen edge survives until the next disable.
    pub(super) fn status_since(&self, now_ns: u64, generation: u64) -> (bool, u64, u64) {
        let state = self.state.lock();
        if state.disable_generation != generation {
            return (false, state.base_sequence, state.last_edge_ns);
        }
        let (sequence, edge_ns) = state.at(now_ns);
        (state.active, sequence, edge_ns)
    }

    pub(super) fn deadline_ns(&self, sequence: u64, now_ns: u64) -> Option<u64> {
        let state = self.state.lock();
        if !state.active {
            return None;
        }
        let (current, _) = state.at(now_ns);
        if sequence <= current && !vblank_passed(current, sequence) {
            return None;
        }
        Some(state.edge_ns_of(sequence))
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
        // A nearby target just before wrap has passed; a future target has not.
        assert!(vblank_passed(1, u64::MAX));
        assert!(!vblank_passed(1, 2));
        assert!(vblank_passed(1 << 23, 0));
        assert!(!vblank_passed((1 << 23) + 1, 0));
    }

    #[test]
    fn widen_resolves_low_values_near_reference() {
        // Plain small value near a small counter: unchanged.
        assert_eq!(widen_32_to_64(5, 0), 5);
        // Value in the same high-word neighborhood as the reference.
        assert_eq!(widen_32_to_64(5, 0x1_0000_0005), 0x1_0000_0005);
        // Values resolve to the closest wrap on either side of the reference.
        assert_eq!(widen_32_to_64(0xffff_fff0, 0), u64::MAX - 15);
        assert_eq!(widen_32_to_64(5, 0xffff_fff0), 0x1_0000_0005);
        assert_eq!(widen_32_to_64(0xffff_fff0, 0x2_0000_0000), 0x1_ffff_fff0);
    }

    #[test]
    fn clock_sequences_track_elapsed_periods() {
        let clock = VblankClock::new(1_000);
        assert_eq!(clock.snapshot_at(1_000 + VBLANK_PERIOD_NS * 3).0, 0);
        clock.set_active(true, 1_000);
        assert_eq!(clock.snapshot_at(1_000).0, 0);
        // Just before the first edge.
        assert_eq!(clock.snapshot_at(1_000 + VBLANK_PERIOD_NS - 1).0, 0);
        // On the first edge.
        assert_eq!(clock.snapshot_at(1_000 + VBLANK_PERIOD_NS).0, 1);
        // Three and a half periods later.
        assert_eq!(
            clock.snapshot_at(1_000 + VBLANK_PERIOD_NS * 7 / 2).0,
            3
        );
        // Edge timestamps round-trip through the sequence computation.
        assert_eq!(clock.deadline_ns(4, 1_000), Some(1_000 + VBLANK_PERIOD_NS * 4));

        clock.set_active(false, 1_000 + VBLANK_PERIOD_NS * 7 / 2);
        assert_eq!(clock.snapshot_at(1_000 + VBLANK_PERIOD_NS * 30).0, 3);
        assert_eq!(clock.deadline_ns(4, 1_000 + VBLANK_PERIOD_NS * 30), None);
        clock.set_active(true, 1_000 + VBLANK_PERIOD_NS * 30);
        assert_eq!(clock.snapshot_at(1_000 + VBLANK_PERIOD_NS * 30).0, 3);
        assert_eq!(clock.snapshot_at(1_000 + VBLANK_PERIOD_NS * 30).1, 1_000 + VBLANK_PERIOD_NS * 3);
        assert_eq!(
            clock.deadline_ns(4, 1_000 + VBLANK_PERIOD_NS * 30),
            Some(1_000 + VBLANK_PERIOD_NS * 31)
        );
        assert_eq!(clock.snapshot_at(1_000 + VBLANK_PERIOD_NS * 31).0, 4);

        let stale_clock = VblankClock::new(1_000);
        stale_clock.set_active(true, 1_000);
        let stale_at = 1_000 + VBLANK_PERIOD_NS * ((1 << 23) + 1);
        assert_eq!(stale_clock.deadline_ns(0, stale_at), None);
    }

    #[test]
    fn waiter_observes_disable_after_crtc_is_reenabled() {
        let clock = VblankClock::new(1_000);
        clock.set_active(true, 1_000);
        let generation = clock.disable_generation();

        let disabled_at = 1_000 + VBLANK_PERIOD_NS / 2;
        clock.set_active(false, disabled_at);
        let frozen = clock.snapshot_at(disabled_at);
        assert_eq!(frozen, (0, 1_000));
        clock.set_active(true, disabled_at + VBLANK_PERIOD_NS * 10);

        let (active_for_wait, sequence, edge_ns) =
            clock.status_since(disabled_at + VBLANK_PERIOD_NS * 11, generation);
        assert!(!active_for_wait);
        assert_eq!((sequence, edge_ns), frozen);
    }
}
