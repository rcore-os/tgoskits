//! Pure admission state for one task's scheduler-visible perf counters.

/// Why a task perf context rejected a new counter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PerfAttachError {
    /// Task exit already tombstoned the context.
    Closed,
    /// The fixed scheduler-visible storage is full.
    Full,
}

/// Lock-protected state shared by the task exit and event-open transactions.
pub(crate) struct PerfTaskContextState<T, const CAPACITY: usize> {
    accepting: bool,
    counters: heapless::Vec<T, CAPACITY>,
}

impl<T, const CAPACITY: usize> PerfTaskContextState<T, CAPACITY> {
    /// Creates one live, empty task context.
    pub(crate) const fn new() -> Self {
        Self {
            accepting: true,
            counters: heapless::Vec::new(),
        }
    }

    /// Publishes one counter while admission remains open.
    pub(crate) fn attach(&mut self, counter: T) -> Result<(), PerfAttachError> {
        if !self.accepting {
            return Err(PerfAttachError::Closed);
        }
        self.counters
            .push(counter)
            .map_err(|_| PerfAttachError::Full)
    }

    /// Tombstones admission and returns every counter visible at that point.
    pub(crate) fn close_snapshot(&mut self) -> heapless::Vec<T, CAPACITY>
    where
        T: Clone,
    {
        self.accepting = false;
        self.counters.clone()
    }

    /// Returns a bounded snapshot without changing admission state.
    pub(crate) fn snapshot(&self) -> heapless::Vec<T, CAPACITY>
    where
        T: Clone,
    {
        self.counters.clone()
    }

    /// Returns a bounded snapshot only while child inheritance is admissible.
    pub(crate) fn snapshot_if_accepting(&self) -> Option<heapless::Vec<T, CAPACITY>>
    where
        T: Clone,
    {
        self.accepting.then(|| self.counters.clone())
    }

    /// Removes entries that no longer belong in the scheduler-visible set.
    pub(crate) fn retain(&mut self, keep: impl FnMut(&T) -> bool) {
        self.counters.retain(keep);
    }

    /// Removes one entry selected by an identity predicate.
    pub(crate) fn remove(&mut self, matches: impl FnMut(&T) -> bool) -> bool {
        let Some(index) = self.counters.iter().position(matches) else {
            return false;
        };
        self.counters.swap_remove(index);
        true
    }

    /// Borrows the fixed counter slice while its external lock is held.
    pub(crate) fn counters(&self) -> &[T] {
        self.counters.as_slice()
    }
}
