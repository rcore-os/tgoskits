//! Generation boundary for task-interruption publication and acknowledgement.

use core::sync::atomic::{AtomicU64, Ordering};

/// One owner-side observation of the interruption state.
#[derive(Clone, Copy)]
pub(crate) struct InterruptSnapshot(u64);

/// Sticky interruption publication shared by remote producers and one task.
pub(crate) struct InterruptState {
    published: AtomicU64,
    acknowledged: AtomicU64,
}

impl InterruptState {
    pub(crate) const fn new() -> Self {
        Self {
            published: AtomicU64::new(0),
            acknowledged: AtomicU64::new(0),
        }
    }

    /// Publishes the reason before the caller wakes the scheduler thread.
    pub(crate) fn publish(&self) {
        self.published
            .try_update(Ordering::Release, Ordering::Relaxed, |epoch| {
                epoch.checked_add(1)
            })
            .expect("task interruption generation exhausted");
    }

    /// Consumes the interruption currently visible to an interruptible wait.
    pub(crate) fn consume(&self) -> bool {
        let snapshot = self.snapshot();
        self.acknowledge(snapshot)
    }

    pub(crate) fn is_pending(&self) -> bool {
        self.published.load(Ordering::Acquire) > self.acknowledged.load(Ordering::Acquire)
    }

    /// Captures the publications covered by the following owner-side scan.
    pub(crate) fn snapshot(&self) -> InterruptSnapshot {
        InterruptSnapshot(self.published.load(Ordering::Acquire))
    }

    /// Acknowledges the publications covered by `snapshot`.
    ///
    /// Returns whether the snapshot advanced the acknowledged generation.
    pub(crate) fn acknowledge(&self, snapshot: InterruptSnapshot) -> bool {
        let mut acknowledged = self.acknowledged.load(Ordering::Acquire);
        while acknowledged < snapshot.0 {
            match self.acknowledged.compare_exchange_weak(
                acknowledged,
                snapshot.0,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(current) => acknowledged = current,
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::InterruptState;

    #[test]
    fn publication_after_snapshot_survives_acknowledgement() {
        let state = InterruptState::new();
        let scanned = state.snapshot();

        state.publish();
        let _advanced = state.acknowledge(scanned);

        assert!(
            state.is_pending(),
            "acknowledging an older scan must not erase a later publication"
        );
    }

    #[test]
    fn snapshot_acknowledges_only_visible_publications() {
        let state = InterruptState::new();
        state.publish();

        let scanned = state.snapshot();
        assert!(state.acknowledge(scanned));
        assert!(!state.is_pending());
    }

    #[test]
    fn older_acknowledgement_cannot_regress_a_consumer() {
        let state = InterruptState::new();
        let old = state.snapshot();
        state.publish();

        assert!(state.consume());
        assert!(!state.acknowledge(old));
        assert!(!state.is_pending());
    }
}
