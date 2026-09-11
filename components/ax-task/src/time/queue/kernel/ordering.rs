//! Ordering under the owning scheduler transaction.

use super::*;

impl KernelTimerQueue {
    pub(super) fn insert_at(&mut self, entry: KernelTimerEntry) {
        debug_assert!(entry.is_armed());
        let position = self.active.partition_point(|candidate| {
            (candidate.deadline(), candidate.identity()) > (entry.deadline(), entry.identity())
        });
        self.active.insert(position, entry);
    }

    pub(super) fn insert_entry(&mut self, entry: KernelTimerEntry) {
        if entry.is_armed() {
            self.insert_at(entry);
        } else {
            self.inactive.push(entry);
        }
    }

    pub(crate) fn next_soft_deadline(&self) -> Option<MonotonicDeadline> {
        self.next_active_entry(false)
            .map(KernelTimerEntry::deadline)
    }

    pub(crate) fn next_hard_deadline(&self) -> Option<MonotonicDeadline> {
        self.next_active_entry(true).map(KernelTimerEntry::deadline)
    }

    pub(crate) fn has_due_soft(&self, now: MonotonicInstant) -> bool {
        self.next_soft_deadline()
            .is_some_and(|deadline| now.reached(deadline))
    }

    pub(crate) fn has_expired(&self) -> bool {
        !self.expired.is_empty()
    }

    pub(crate) fn has_completed(&self) -> bool {
        !self.completed.is_empty()
    }

    #[cfg(test)]
    pub(super) fn has_inactive(&self) -> bool {
        !self.inactive.is_empty()
    }

    pub(crate) fn has_active_work(&self) -> bool {
        !self.active.is_empty()
            || !self.expired.is_empty()
            || !self.executing.is_empty()
            || !self.completed.is_empty()
    }

    pub(super) fn next_active_index(&self, hard: bool) -> Option<usize> {
        self.active
            .iter()
            .rposition(|entry| entry.is_hard() == hard)
    }

    pub(super) fn next_active_entry(&self, hard: bool) -> Option<&KernelTimerEntry> {
        self.next_active_index(hard)
            .map(|index| &self.active[index])
    }
}
