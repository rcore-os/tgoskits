//! Generation-bearing exit callback and reap candidate ownership.

use alloc::collections::VecDeque;

use crate::ThreadId;

/// Exit work published exactly once by each occupied registry slot.
#[derive(Debug)]
pub(super) struct ExitedThreadWork {
    candidates: VecDeque<ThreadId>,
}

impl ExitedThreadWork {
    pub(super) const fn new() -> Self {
        Self {
            candidates: VecDeque::new(),
        }
    }

    /// Reserves candidate capacity while thread construction may allocate.
    pub(super) fn reserve_slot_capacity(&mut self, slot_count: usize) {
        self.candidates
            .reserve(slot_count.saturating_sub(self.candidates.len()));
    }

    /// Publishes an exited generation without allocating in the exit path.
    pub(super) fn publish(&mut self, thread: ThreadId, slot_count: usize) {
        debug_assert!(
            self.candidates.len() < slot_count,
            "each occupied slot can publish at most one exit candidate"
        );
        self.candidates.push_back(thread);
    }

    pub(super) fn candidate_count(&self) -> usize {
        self.candidates.len()
    }

    /// Rotates one candidate so a busy zombie cannot starve later work.
    pub(super) fn next_candidate(&mut self) -> Option<ThreadId> {
        let thread = self.candidates.pop_front()?;
        self.candidates.push_back(thread);
        Some(thread)
    }

    pub(super) fn remove(&mut self, thread: ThreadId) {
        self.candidates.retain(|candidate| *candidate != thread);
    }

    #[cfg(test)]
    pub(super) fn capacity(&self) -> usize {
        self.candidates.capacity()
    }
}
