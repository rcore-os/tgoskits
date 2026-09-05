//! Fair wake-affinity relationship tracking.

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::{ThreadCore, ThreadId, runtime::MonotonicInstant};

const NO_WAKEE: u64 = u64::MAX;
const WAKEE_FLIP_DECAY_NS: u64 = 1_000_000_000;

/// Per-thread state corresponding to Linux `task_struct::{wakee_flips,
/// wakee_flip_decay_ts,last_wakee}`.
#[derive(Debug)]
pub(super) struct WakeAffinityState {
    wakee_flips: AtomicU32,
    wakee_flip_decay_ns: AtomicU64,
    last_wakee: AtomicU64,
}

impl WakeAffinityState {
    pub(super) const fn new() -> Self {
        Self {
            wakee_flips: AtomicU32::new(0),
            wakee_flip_decay_ns: AtomicU64::new(0),
            last_wakee: AtomicU64::new(NO_WAKEE),
        }
    }

    /// Mirrors Linux `record_wakee()`.
    ///
    /// These atomics publish only a placement heuristic. They do not order
    /// task state: the current execution context is the sole ordinary writer,
    /// while relaxed operations also keep nested IRQ wakeups data-race free.
    fn record_wakee(&self, wakee: ThreadId, now: MonotonicInstant) -> u32 {
        let now_ns = now.as_nanos();
        let decay_ns = self.wakee_flip_decay_ns.load(Ordering::Relaxed);
        if now_ns > decay_ns.saturating_add(WAKEE_FLIP_DECAY_NS) {
            self.wakee_flips
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |flips| {
                    Some(flips >> 1)
                })
                .expect("wakee flip decay always supplies a replacement");
            self.wakee_flip_decay_ns.store(now_ns, Ordering::Relaxed);
        }

        if self.last_wakee.swap(wakee.as_u64(), Ordering::Relaxed) != wakee.as_u64() {
            self.wakee_flips.fetch_add(1, Ordering::Relaxed);
        }
        self.wakee_flips.load(Ordering::Relaxed)
    }

    fn wakee_flips(&self) -> u32 {
        self.wakee_flips.load(Ordering::Relaxed)
    }
}

/// Reports whether a waker/wakee relationship is wide enough to skip
/// wake-affine placement.
pub(super) fn is_wide_wake_relationship(waker_flips: u32, wakee_flips: u32, llc_size: u32) -> bool {
    let (master, slave) = if waker_flips < wakee_flips {
        (wakee_flips, waker_flips)
    } else {
        (waker_flips, wakee_flips)
    };
    slave >= llc_size && u64::from(master) >= u64::from(slave) * u64::from(llc_size)
}

impl ThreadCore {
    /// Mirrors Linux `record_wakee()` followed by `wake_wide()` while the
    /// caller retains the current execution context.
    pub(crate) fn record_wakee_and_is_wide(
        &self,
        wakee: &Self,
        now: MonotonicInstant,
        llc_size: u32,
    ) -> bool {
        debug_assert_ne!(llc_size, 0);
        let waker_flips = self.wake_affinity.record_wakee(wakee.id(), now);
        is_wide_wake_relationship(waker_flips, wakee.wake_affinity.wakee_flips(), llc_size)
    }
}

#[cfg(test)]
mod tests {
    use super::{WAKEE_FLIP_DECAY_NS, WakeAffinityState, is_wide_wake_relationship};
    use crate::{ThreadId, runtime::MonotonicInstant};

    #[test]
    fn linux_wake_wide_requires_both_partner_thresholds() {
        assert!(!is_wide_wake_relationship(8, 2, 4));
        assert!(is_wide_wake_relationship(16, 4, 4));
        assert!(is_wide_wake_relationship(4, 16, 4));
    }

    #[test]
    fn linux_record_wakee_counts_switches_and_decays_after_one_second() {
        let state = WakeAffinityState::new();
        let first = ThreadId::from_parts(1, 1);
        let second = ThreadId::from_parts(2, 1);
        let at = |nanos| MonotonicInstant::from_nanos(nanos).unwrap();

        assert_eq!(state.record_wakee(first, at(0)), 1);
        assert_eq!(state.record_wakee(first, at(1)), 1);
        assert_eq!(state.record_wakee(second, at(2)), 2);
        assert_eq!(state.record_wakee(first, at(3)), 3);
        assert_eq!(state.record_wakee(second, at(4)), 4);
        assert_eq!(state.record_wakee(second, at(WAKEE_FLIP_DECAY_NS)), 4);
        assert_eq!(state.record_wakee(first, at(WAKEE_FLIP_DECAY_NS + 1)), 3);
    }
}
