//! RT-lock overlay on the one atomic lifecycle publication.

use super::*;

impl ThreadLifecycle {
    /// Overlays an RT-lock park while preserving the outer wait publication.
    pub(crate) fn enter_rt_lock_wait(&self) -> Result<(), TaskError> {
        let mut observed = self.state.load(Ordering::Acquire);
        loop {
            let current = decode_state(observed);
            if observed & RTLOCK_ACTIVE != 0
                || !matches!(current, ThreadState::Running | ThreadState::Parking)
            {
                return Err(TaskError::InvalidConfiguration);
            }
            let updated = overlay_rt_lock_state(observed);
            match self.state.compare_exchange_weak(
                observed,
                updated,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(()),
                Err(next) => observed = next,
            }
        }
    }

    pub(crate) fn in_rt_lock_wait(&self) -> bool {
        self.state.load(Ordering::Acquire) & RTLOCK_ACTIVE != 0
    }

    /// Restores the outer park and every ordinary wake which raced lock acquisition.
    pub(crate) fn restore_rt_lock_wait(&self) -> Result<(), TaskError> {
        let mut observed = self.state.load(Ordering::Acquire);
        loop {
            if observed & RTLOCK_ACTIVE == 0 || decode_state(observed) != ThreadState::Running {
                return Err(TaskError::InvalidConfiguration);
            }
            let restored = observed >> SAVED_SHIFT;
            match self.state.compare_exchange_weak(
                observed,
                restored,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(()),
                Err(next) => observed = next,
            }
        }
    }

    /// Only a lock-specific handoff may wake the inner RT-lock park.
    pub(crate) fn publish_rt_lock_wake(&self) -> Option<WakePublication> {
        let previous = self
            .state
            .try_update(Ordering::AcqRel, Ordering::Acquire, |observed| {
                (observed & RTLOCK_ACTIVE != 0).then_some(observed | WAKE_STATE_PUBLISHED)
            })
            .ok()?;
        Some(WakePublication {
            state: decode_state(previous),
            already_pending: previous & WAKE_PENDING != 0,
            saved_state_only: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_wake_is_saved_until_rt_lock_handoff() {
        let state = ThreadLifecycle::new();
        state.transition(ThreadState::Running).unwrap();
        state.transition(ThreadState::Parking).unwrap();
        state.enter_rt_lock_wait().unwrap();
        assert!(state.in_rt_lock_wait());
        state.transition(ThreadState::Parking).unwrap();
        assert_eq!(
            state.publish_blocked_from_parking().unwrap(),
            ParkPublication::Blocked
        );
        let wake = state.publish_wake();
        assert!(wake.saved_state_only());
        assert_eq!(state.state(), ThreadState::Blocked);
        assert!(!state.consume_wake(false));
        assert!(state.publish_wake().already_pending());

        let handoff = state.publish_rt_lock_wake().unwrap();
        assert!(!handoff.saved_state_only());
        assert_eq!(
            state.consume_wake_and_transition(false, Some(ThreadState::Waking)),
            (ThreadState::Blocked, true)
        );
        state.transition(ThreadState::Running).unwrap();
        state.restore_rt_lock_wait().unwrap();
        assert!(!state.in_rt_lock_wait());
        assert_eq!(state.state(), ThreadState::Parking);
        assert_eq!(
            state.publish_blocked_from_parking().unwrap(),
            ParkPublication::Notified
        );
    }

    #[test]
    fn rt_lock_wake_does_not_notify_an_ordinary_wait() {
        let state = ThreadLifecycle::new();
        state.transition(ThreadState::Running).unwrap();
        assert!(state.publish_rt_lock_wake().is_none());
        assert!(!state.take_park_notification());
        state.publish_wake();
        state.enter_rt_lock_wait().unwrap();
        assert!(!state.take_park_notification());
        state.restore_rt_lock_wait().unwrap();
        assert!(state.take_park_notification());
        assert!(!state.take_park_notification());
    }

    #[test]
    fn failed_inner_wake_does_not_clear_the_saved_notification() {
        let state = ThreadLifecycle::new();
        state.transition(ThreadState::Running).unwrap();
        state.enter_rt_lock_wait().unwrap();
        state.publish_wake();
        state.publish_rt_lock_wake().unwrap();
        state.discard_failed_wake();
        state.restore_rt_lock_wait().unwrap();
        assert!(state.take_park_notification());
    }

    #[test]
    fn nested_overlay_is_rejected_without_corrupting_the_outer_wait() {
        let state = ThreadLifecycle::new();
        state.transition(ThreadState::Running).unwrap();
        state.enter_rt_lock_wait().unwrap();
        assert_eq!(
            state.enter_rt_lock_wait(),
            Err(TaskError::InvalidConfiguration)
        );
        state.restore_rt_lock_wait().unwrap();
        assert_eq!(state.state(), ThreadState::Running);
    }
}

#[cfg(all(test, not(miri)))]
mod loom_tests {
    use loom::sync::{Arc, atomic::AtomicU16};

    use super::*;

    #[test]
    fn ordinary_notification_survives_rt_lock_entry_and_restore() {
        loom::model(|| {
            let publication = Arc::new(AtomicU16::new(ThreadState::Running as u16));
            let wake_state = Arc::clone(&publication);
            let waker = loom::thread::spawn(move || {
                wake_state
                    .fetch_update(Ordering::AcqRel, Ordering::Acquire, |observed| {
                        Some(publish_ordinary_notification(observed))
                    })
                    .unwrap();
            });
            publication
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |observed| {
                    Some(overlay_rt_lock_state(observed))
                })
                .unwrap();
            publication
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |observed| {
                    Some(observed >> SAVED_SHIFT)
                })
                .unwrap();
            waker.join().unwrap();
            let final_state = publication.load(Ordering::Acquire);
            assert_eq!(final_state & WAKE_STATE_PUBLISHED, WAKE_STATE_PUBLISHED);
            assert_eq!(decode_state(final_state), ThreadState::Running);
        });
    }
}
