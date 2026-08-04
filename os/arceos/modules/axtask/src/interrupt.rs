//! Task interruption publication state.

use core::sync::atomic::{AtomicU64, Ordering};

/// A task interruption observation captured before an owner-side safe-point scan.
///
/// Obtain a snapshot through [`crate::TaskInner::interrupt_snapshot`] and pass it
/// to [`crate::TaskInner::acknowledge_interrupt`] after that scan completes.
#[derive(Debug, Clone, Copy)]
pub struct InterruptSnapshot(u64);

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

    pub(crate) fn publish(&self) {
        self.published
            .try_update(Ordering::Release, Ordering::Relaxed, |generation| {
                generation.checked_add(1)
            })
            .expect("task interruption generation exhausted");
    }

    pub(crate) fn consume(&self) -> bool {
        self.acknowledge(self.snapshot())
    }

    pub(crate) fn is_pending(&self) -> bool {
        self.published.load(Ordering::Acquire) > self.acknowledged.load(Ordering::Acquire)
    }

    pub(crate) fn snapshot(&self) -> InterruptSnapshot {
        InterruptSnapshot(self.published.load(Ordering::Acquire))
    }

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
        state.publish();
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
