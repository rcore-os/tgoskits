//! Checked thread lifecycle transitions.

use core::sync::atomic::{AtomicU16, Ordering};

use crate::thread::{TaskError, ThreadState};

#[path = "state/rt_lock.rs"]
mod rt_lock;

const STATE_MASK: u16 = 0b111;
const WAKE_PENDING: u16 = 1 << 3;
const PARK_NOTIFIED: u16 = 1 << 4;
const WAKE_STATE_PUBLISHED: u16 = WAKE_PENDING | PARK_NOTIFIED;
const RTLOCK_ACTIVE: u16 = 1 << 5;
const SAVED_SHIFT: u32 = 8;

const fn publish_ordinary_notification(observed: u16) -> u16 {
    let bits = if observed & RTLOCK_ACTIVE != 0 {
        WAKE_STATE_PUBLISHED << SAVED_SHIFT
    } else {
        WAKE_STATE_PUBLISHED
    };
    observed | bits
}

const fn overlay_rt_lock_state(observed: u16) -> u16 {
    RTLOCK_ACTIVE | (observed << SAVED_SHIFT) | ThreadState::Running as u16
}

/// Single atomic publication for task lifecycle and wake/schedule races.
#[derive(Debug)]
pub(crate) struct ThreadLifecycle {
    state: AtomicU16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct WakePublication {
    state: ThreadState,
    already_pending: bool,
    saved_state_only: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ParkPublication {
    Notified,
    Blocked,
}

impl WakePublication {
    pub(crate) const fn saved_state_only(self) -> bool {
        self.saved_state_only
    }

    pub(crate) const fn state(self) -> ThreadState {
        self.state
    }

    pub(crate) const fn already_pending(self) -> bool {
        self.already_pending
    }
}

impl ThreadLifecycle {
    pub(crate) const fn new() -> Self {
        Self {
            state: AtomicU16::new(ThreadState::New as u16),
        }
    }

    #[track_caller]
    pub(crate) fn state(&self) -> ThreadState {
        decode_state(self.state.load(Ordering::Acquire))
    }

    #[track_caller]
    pub(crate) fn transition(&self, next: ThreadState) -> Result<(), TaskError> {
        let mut observed = self.state.load(Ordering::Acquire);
        loop {
            let current = decode_state(observed);
            if !transition_is_valid(current, next) {
                return Err(TaskError::InvalidTransition {
                    from: current,
                    to: next,
                });
            }
            // Admission is the boundary between pre-publication notifications
            // and runnable-thread wakes. Clear the former in this same CAS for
            // every New transition, including bootstrap. A racing wake that
            // observes Running publishes after this CAS and remains pending.
            let retained = if current == ThreadState::New {
                0
            } else {
                observed & !STATE_MASK
            };
            let updated = retained | next as u16;
            match self.state.compare_exchange_weak(
                observed,
                updated,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(()),
                Err(updated) => observed = updated,
            }
        }
    }

    #[track_caller]
    pub(crate) fn publish_wake(&self) -> WakePublication {
        // One CAS owns the choice of active versus saved notification bits.
        // A separate flag load followed by fetch_or would race RT-lock entry.
        let previous = self
            .state
            .try_update(Ordering::AcqRel, Ordering::Acquire, |observed| {
                Some(publish_ordinary_notification(observed))
            })
            .expect("wake publication cannot be rejected");
        let saved_state_only = previous & RTLOCK_ACTIVE != 0;
        let notification = if saved_state_only {
            previous >> SAVED_SHIFT
        } else {
            previous
        };
        WakePublication {
            state: decode_state(previous),
            already_pending: notification & WAKE_PENDING != 0,
            saved_state_only,
        }
    }

    #[cfg(test)]
    pub(crate) fn consume_wake(&self, preserve_park_notification: bool) -> bool {
        let consumed = if preserve_park_notification {
            WAKE_PENDING
        } else {
            WAKE_STATE_PUBLISHED
        };
        self.state.fetch_and(!consumed, Ordering::AcqRel) & WAKE_PENDING != 0
    }

    pub(crate) fn discard_failed_wake(&self) {
        self.state
            .fetch_and(!WAKE_STATE_PUBLISHED, Ordering::AcqRel);
    }

    pub(crate) fn take_park_notification(&self) -> bool {
        self.state
            .fetch_and(!WAKE_STATE_PUBLISHED, Ordering::AcqRel)
            & PARK_NOTIFIED
            != 0
    }

    /// Consumes one wake publication and optionally advances a blocked task in
    /// the same lifecycle CAS. The task lock serializes scheduler ownership,
    /// while this single atomic update closes the remaining park/wake race
    /// without publishing an intermediate `Blocked` observation.
    pub(crate) fn consume_wake_and_transition(
        &self,
        preserve_park_notification: bool,
        next: Option<ThreadState>,
    ) -> (ThreadState, bool) {
        let mut observed = self.state.load(Ordering::Acquire);
        loop {
            let current = decode_state(observed);
            let pending = observed & WAKE_PENDING != 0;
            let consumed = if preserve_park_notification && current == ThreadState::Parking {
                WAKE_PENDING
            } else {
                WAKE_STATE_PUBLISHED
            };
            let mut updated = observed & !consumed;
            if pending && next == Some(ThreadState::Waking) && current == ThreadState::Blocked {
                updated = (updated & !STATE_MASK) | ThreadState::Waking as u16;
            }
            if updated == observed {
                return (current, pending);
            }
            match self.state.compare_exchange_weak(
                observed,
                updated,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return (current, pending),
                Err(next_observed) => observed = next_observed,
            }
        }
    }

    /// Atomically chooses between a racing wake and blocked publication.
    #[track_caller]
    pub(crate) fn publish_blocked_from_parking(&self) -> Result<ParkPublication, TaskError> {
        let mut observed = self.state.load(Ordering::Acquire);
        loop {
            let current = decode_state(observed);
            if current != ThreadState::Parking {
                return Err(TaskError::InvalidTransition {
                    from: current,
                    to: ThreadState::Blocked,
                });
            }
            let (updated, publication) = if observed & PARK_NOTIFIED != 0 {
                (
                    (observed & !(STATE_MASK | WAKE_STATE_PUBLISHED)) | ThreadState::Running as u16,
                    ParkPublication::Notified,
                )
            } else {
                (
                    (observed & !(STATE_MASK | WAKE_STATE_PUBLISHED)) | ThreadState::Blocked as u16,
                    ParkPublication::Blocked,
                )
            };
            match self.state.compare_exchange_weak(
                observed,
                updated,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(publication),
                Err(updated) => observed = updated,
            }
        }
    }
}

#[track_caller]
pub(crate) fn decode_state(packed: u16) -> ThreadState {
    match packed & STATE_MASK {
        0 => ThreadState::New,
        2 => ThreadState::Running,
        3 => ThreadState::Parking,
        4 => ThreadState::Blocked,
        5 => ThreadState::Waking,
        6 => ThreadState::Exited,
        _ => panic!("invalid thread lifecycle publication: raw={packed:#04x}"),
    }
}

pub(crate) const fn transition_is_valid(from: ThreadState, to: ThreadState) -> bool {
    matches!(
        (from, to),
        (ThreadState::New, ThreadState::Running | ThreadState::Exited)
            | (
                ThreadState::Running,
                ThreadState::Parking | ThreadState::Exited
            )
            | (
                ThreadState::Parking,
                ThreadState::Running | ThreadState::Blocked | ThreadState::Waking
            )
            | (
                ThreadState::Blocked,
                // Linux `ttwu_runnable()` changes an on-rq sleeper directly
                // to TASK_RUNNING. Only the off-rq enqueue path uses Waking.
                ThreadState::Running | ThreadState::Waking | ThreadState::Exited
            )
            | (
                ThreadState::Waking,
                ThreadState::Running | ThreadState::Exited
            )
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_documented_wake_transition() {
        let lifecycle = ThreadLifecycle::new();
        lifecycle.transition(ThreadState::Running).unwrap();
        lifecycle.transition(ThreadState::Parking).unwrap();
        lifecycle.transition(ThreadState::Waking).unwrap();
        lifecycle.transition(ThreadState::Running).unwrap();
        assert_eq!(lifecycle.state(), ThreadState::Running);
    }

    #[test]
    fn rejects_runnable_to_blocked_shortcut() {
        assert!(!transition_is_valid(
            ThreadState::Running,
            ThreadState::Blocked
        ));
    }

    #[test]
    fn wake_publication_atomically_defeats_blocked_publication() {
        let lifecycle = ThreadLifecycle::new();
        lifecycle.transition(ThreadState::Running).unwrap();
        lifecycle.transition(ThreadState::Parking).unwrap();
        assert_eq!(lifecycle.publish_wake().state(), ThreadState::Parking);
        assert_eq!(
            lifecycle.publish_blocked_from_parking().unwrap(),
            ParkPublication::Notified
        );
        assert_eq!(lifecycle.state(), ThreadState::Running);
        assert!(!lifecycle.consume_wake(false));
    }

    #[test]
    fn blocked_publication_wins_before_late_wake() {
        let lifecycle = ThreadLifecycle::new();
        lifecycle.transition(ThreadState::Running).unwrap();
        lifecycle.transition(ThreadState::Parking).unwrap();
        assert_eq!(
            lifecycle.publish_blocked_from_parking().unwrap(),
            ParkPublication::Blocked
        );
        assert_eq!(lifecycle.publish_wake().state(), ThreadState::Blocked);
    }

    #[test]
    fn on_rq_wake_transitions_directly_from_blocked_to_running() {
        let lifecycle = ThreadLifecycle::new();
        lifecycle.transition(ThreadState::Running).unwrap();
        lifecycle.transition(ThreadState::Parking).unwrap();
        assert_eq!(
            lifecycle.publish_blocked_from_parking().unwrap(),
            ParkPublication::Blocked
        );

        lifecycle.transition(ThreadState::Running).unwrap();

        assert_eq!(lifecycle.state(), ThreadState::Running);
    }

    #[test]
    #[should_panic(expected = "invalid thread lifecycle publication: raw=0x0f")]
    fn invalid_lifecycle_publication_reports_the_packed_byte() {
        let lifecycle = ThreadLifecycle::new();
        lifecycle
            .state
            .store(WAKE_PENDING | STATE_MASK, Ordering::Relaxed);

        let _ = lifecycle.state();
    }

    #[test]
    #[should_panic(expected = "invalid thread lifecycle publication: raw=0x01")]
    fn reserved_state_encoding_is_rejected() {
        let lifecycle = ThreadLifecycle::new();
        lifecycle.state.store(1, Ordering::Relaxed);

        let _ = lifecycle.state();
    }
}

#[cfg(all(test, not(miri)))]
mod loom_tests {
    use loom::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
    };

    const STATE_MASK: usize = 0b111;
    const RUNNING: usize = 2;
    const PARKING: usize = 3;
    const BLOCKED: usize = 4;
    const WAKE_PENDING: usize = 1 << 3;
    const PARK_NOTIFIED: usize = 1 << 4;
    const WAKE_STATE_PUBLISHED: usize = WAKE_PENDING | PARK_NOTIFIED;

    #[test]
    fn wake_publication_cannot_strand_a_parking_thread() {
        loom::model(|| {
            let lifecycle = Arc::new(AtomicUsize::new(PARKING));

            let parker = {
                let lifecycle = Arc::clone(&lifecycle);
                thread::spawn(move || {
                    let mut observed = lifecycle.load(Ordering::Acquire);
                    loop {
                        assert_eq!(observed & STATE_MASK, PARKING);
                        let updated = if observed & PARK_NOTIFIED != 0 {
                            RUNNING
                        } else {
                            BLOCKED
                        };
                        match lifecycle.compare_exchange_weak(
                            observed,
                            updated,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        ) {
                            Ok(_) => break,
                            Err(updated) => observed = updated,
                        }
                    }
                })
            };
            let waker = {
                let lifecycle = Arc::clone(&lifecycle);
                thread::spawn(move || {
                    let previous = lifecycle.fetch_or(WAKE_STATE_PUBLISHED, Ordering::AcqRel);
                    if previous & STATE_MASK == BLOCKED {
                        let observed = lifecycle.fetch_and(!WAKE_STATE_PUBLISHED, Ordering::AcqRel);
                        assert_ne!(observed & WAKE_PENDING, 0);
                        lifecycle
                            .compare_exchange(BLOCKED, RUNNING, Ordering::AcqRel, Ordering::Acquire)
                            .unwrap();
                    }
                })
            };

            parker.join().unwrap();
            waker.join().unwrap();
            assert_ne!(
                lifecycle.load(Ordering::Acquire) & STATE_MASK,
                BLOCKED,
                "a wake racing Parking-to-Blocked must resume or activate the thread"
            );
        });
    }
}
