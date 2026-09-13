//! Registration under the owning scheduler transaction.

use super::*;

impl KernelTimerQueue {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            active: Vec::with_capacity(capacity),
            inactive: Vec::with_capacity(capacity),
            expired: Vec::with_capacity(capacity),
            executing: Vec::with_capacity(capacity),
            completed: Vec::with_capacity(capacity),
            capacity,
        }
    }

    pub(crate) fn insert(
        &mut self,
        owner: CpuId,
        entry: KernelTimerEntry,
    ) -> Result<KernelTimerHandle, KernelTimerEntry> {
        if self.active.len()
            + self.inactive.len()
            + self.expired.len()
            + self.executing.len()
            + self.completed.len()
            >= self.capacity
        {
            return Err(entry);
        }
        let handle = KernelTimerHandle::new(owner, entry.identity());
        self.insert_entry(entry);
        Ok(handle)
    }

    pub(crate) fn cancel(
        &mut self,
        handle: KernelTimerHandle,
    ) -> (KernelTimerCancelOutcome, Option<KernelTimerEntry>) {
        if let Some(index) = self
            .active
            .iter()
            .position(|entry| entry.identity() == handle.identity())
        {
            return (
                KernelTimerCancelOutcome::Cancelled,
                Some(self.active.remove(index)),
            );
        }
        if let Some(index) = self
            .inactive
            .iter()
            .position(|entry| entry.identity() == handle.identity())
        {
            return (
                KernelTimerCancelOutcome::Cancelled,
                Some(self.inactive.remove(index)),
            );
        }
        let removed = self
            .expired
            .iter()
            .position(|entry| entry.identity() == handle.identity())
            .map(|index| self.expired.remove(index));
        if removed.is_some() {
            return (KernelTimerCancelOutcome::Cancelled, removed);
        }
        if let Some(executing) = self
            .executing
            .iter_mut()
            .find(|entry| entry.identity == handle.identity())
        {
            executing.disposition = ExecutingKernelTimerDisposition::Destroy;
            return (KernelTimerCancelOutcome::CancellationDeferred, None);
        }
        if self
            .completed
            .iter()
            .any(|entry| entry.identity() == handle.identity())
        {
            return (KernelTimerCancelOutcome::CancellationDeferred, None);
        }
        (KernelTimerCancelOutcome::AlreadyCompleted, None)
    }

    pub(crate) fn arm_hard(
        &mut self,
        handle: KernelTimerHandle,
        deadline: MonotonicDeadline,
    ) -> bool {
        if let Some(index) = self
            .inactive
            .iter()
            .position(|entry| entry.identity() == handle.identity() && entry.is_hard())
        {
            let mut entry = self.inactive.remove(index);
            entry.rearm(deadline);
            self.insert_at(entry);
            return true;
        }
        if let Some(executing) = self
            .executing
            .iter_mut()
            .find(|entry| entry.identity == handle.identity() && entry.hard)
            && executing.disposition != ExecutingKernelTimerDisposition::Destroy
        {
            // Like hrtimer_start() racing a running callback, task context
            // publishes the next arm on the stable identity. Completion owns
            // the only transition back into the active base.
            executing.disposition = ExecutingKernelTimerDisposition::Rearm(deadline);
            return true;
        }
        false
    }

    /// Disarms one stable hard registration without releasing its payload.
    ///
    /// `Some(Some(deadline))` reports an active entry that moved to inactive,
    /// `Some(None)` reports an already inactive or executing entry, and `None`
    /// reports a stale or non-hard handle.
    pub(crate) fn disarm_hard(
        &mut self,
        handle: KernelTimerHandle,
    ) -> Option<Option<MonotonicDeadline>> {
        if self
            .inactive
            .iter()
            .any(|entry| entry.identity() == handle.identity() && entry.is_hard())
        {
            return Some(None);
        }
        if let Some(index) = self
            .active
            .iter()
            .position(|entry| entry.identity() == handle.identity() && entry.is_hard())
        {
            let mut entry = self.active.remove(index);
            let deadline = entry.disarm();
            self.inactive.push(entry);
            return Some(Some(deadline));
        }
        if let Some(executing) = self
            .executing
            .iter_mut()
            .find(|entry| entry.identity == handle.identity() && entry.hard)
        {
            if executing.disposition != ExecutingKernelTimerDisposition::Destroy {
                executing.disposition = ExecutingKernelTimerDisposition::Disarm;
            }
            return Some(None);
        }
        None
    }

    pub(crate) fn restore_cancelled(&mut self, entry: KernelTimerEntry) {
        assert!(
            self.active.len()
                + self.inactive.len()
                + self.expired.len()
                + self.executing.len()
                + self.completed.len()
                < self.capacity,
            "restoring a cancelled kernel timer must reuse its reserved capacity"
        );
        self.insert_entry(entry);
    }
}
