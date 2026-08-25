//! Checked thread lifecycle transitions.

use core::sync::atomic::{AtomicU8, Ordering};

use crate::TaskError;

const STATE_MASK: u8 = 0b111;
const WAKE_PENDING: u8 = 1 << 3;
const PARK_NOTIFIED: u8 = 1 << 4;
const WAKE_STATE_PUBLISHED: u8 = WAKE_PENDING | PARK_NOTIFIED;

/// Observable lifecycle state of a thread.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ThreadState {
    /// Allocated but not admitted to a run queue.
    New     = 0,
    /// Legacy runnable encoding retained for trace and API compatibility.
    ///
    /// The scheduler does not publish this value. New runnable states use
    /// [`ThreadState::Running`] and placement distinguishes queued/current.
    Ready   = 1,
    /// Linux-style `TASK_RUNNING`, whether queued or executing on a CPU.
    Running = 2,
    /// Publishing a block operation while racing with wake-up.
    Parking = 3,
    /// Asleep on a wait object.
    Blocked = 4,
    /// A wake operation won the block/wake race.
    Waking  = 5,
    /// Execution has terminated and resources await reaping.
    Exited  = 6,
}

/// Single atomic publication for task lifecycle and wake/schedule races.
#[derive(Debug)]
pub(crate) struct ThreadLifecycle {
    state: AtomicU8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct WakePublication {
    state: ThreadState,
    already_pending: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ParkPublication {
    Notified,
    Blocked,
}

impl WakePublication {
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
            state: AtomicU8::new(ThreadState::New as u8),
        }
    }

    pub(crate) fn state(&self) -> ThreadState {
        decode_state(self.state.load(Ordering::Acquire) & STATE_MASK)
    }

    pub(crate) fn transition(&self, next: ThreadState) -> Result<(), TaskError> {
        let mut observed = self.state.load(Ordering::Acquire);
        loop {
            let current = decode_state(observed & STATE_MASK);
            if !transition_is_valid(current, next) {
                return Err(TaskError::InvalidTransition {
                    from: current,
                    to: next,
                });
            }
            let updated = (observed & !STATE_MASK) | next as u8;
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

    pub(crate) fn publish_wake(&self) -> WakePublication {
        let previous = self.state.fetch_or(WAKE_STATE_PUBLISHED, Ordering::AcqRel);
        WakePublication {
            state: decode_state(previous & STATE_MASK),
            already_pending: previous & WAKE_PENDING != 0,
        }
    }

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

    /// Rolls back a wake publication that found the thread already runnable.
    ///
    /// Linux `try_to_wake_up()` leaves a runnable task completely untouched
    /// when its state match fails, so no wake residue may survive to abort
    /// the thread's next sleep. The rollback is only valid while the state
    /// field still shows a runnable publication: once a parker owns the word
    /// (`Parking`) or a waker owns activation (`Blocked`), the sticky bits
    /// belong to their protocol and must stay set.
    pub(crate) fn discard_runnable_wake(&self) -> bool {
        let mut observed = self.state.load(Ordering::Acquire);
        loop {
            if !matches!(
                decode_state(observed & STATE_MASK),
                ThreadState::New | ThreadState::Ready | ThreadState::Running | ThreadState::Waking
            ) {
                return false;
            }
            match self.state.compare_exchange_weak(
                observed,
                observed & !WAKE_STATE_PUBLISHED,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(updated) => observed = updated,
            }
        }
    }

    #[cfg(any(test, all(axtest, feature = "axtest")))]
    pub(crate) fn wake_is_pending(&self) -> bool {
        self.state.load(Ordering::Acquire) & WAKE_PENDING != 0
    }

    pub(crate) fn take_park_notification(&self) -> bool {
        self.state
            .fetch_and(!WAKE_STATE_PUBLISHED, Ordering::AcqRel)
            & PARK_NOTIFIED
            != 0
    }

    /// Atomically chooses between a racing wake and blocked publication.
    pub(crate) fn publish_blocked_from_parking(&self) -> Result<ParkPublication, TaskError> {
        let mut observed = self.state.load(Ordering::Acquire);
        loop {
            let current = decode_state(observed & STATE_MASK);
            if current != ThreadState::Parking {
                return Err(TaskError::InvalidTransition {
                    from: current,
                    to: ThreadState::Blocked,
                });
            }
            let (updated, publication) = if observed & PARK_NOTIFIED != 0 {
                (
                    (observed & !(STATE_MASK | WAKE_STATE_PUBLISHED)) | ThreadState::Running as u8,
                    ParkPublication::Notified,
                )
            } else {
                (
                    (observed & !(STATE_MASK | WAKE_STATE_PUBLISHED)) | ThreadState::Blocked as u8,
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

pub(crate) const fn decode_state(state: u8) -> ThreadState {
    match state {
        0 => ThreadState::New,
        1 => ThreadState::Ready,
        2 => ThreadState::Running,
        3 => ThreadState::Parking,
        4 => ThreadState::Blocked,
        5 => ThreadState::Waking,
        6 => ThreadState::Exited,
        _ => panic!("invalid thread lifecycle publication"),
    }
}

pub(crate) const fn transition_is_valid(from: ThreadState, to: ThreadState) -> bool {
    matches!(
        (from, to),
        (ThreadState::New, ThreadState::Running | ThreadState::Exited)
            | (
                ThreadState::Ready,
                ThreadState::Running | ThreadState::Exited
            )
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
                ThreadState::Waking | ThreadState::Exited
            )
            | (
                ThreadState::Waking,
                ThreadState::Running | ThreadState::Exited
            )
    )
}

#[cfg(any(test, all(axtest, feature = "axtest")))]
mod tests {
    use super::*;

    #[cfg_attr(test, test)]
    #[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
    fn accepts_the_documented_wake_transition() {
        let lifecycle = ThreadLifecycle::new();
        lifecycle.transition(ThreadState::Running).unwrap();
        lifecycle.transition(ThreadState::Parking).unwrap();
        lifecycle.transition(ThreadState::Waking).unwrap();
        lifecycle.transition(ThreadState::Running).unwrap();
        assert_eq!(lifecycle.state(), ThreadState::Running);
    }

    #[cfg_attr(test, test)]
    #[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
    fn rejects_runnable_to_blocked_shortcut() {
        assert!(!transition_is_valid(
            ThreadState::Running,
            ThreadState::Blocked
        ));
    }

    #[cfg_attr(test, test)]
    #[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
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
        assert!(!lifecycle.wake_is_pending());
    }

    #[cfg_attr(test, test)]
    #[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
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
}
