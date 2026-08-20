//! Allocation-once binary min-heap for value-owned task deadlines.

use alloc::vec::Vec;

use super::{TaskDeadlineKind, TaskDeadlineNodeId, TaskDeadlineToken};
use crate::{ThreadId, runtime::MonotonicDeadline};

#[derive(Clone, Copy, Debug)]
pub(super) struct TimerEntry {
    deadline: MonotonicDeadline,
    thread: ThreadId,
    token: TaskDeadlineToken,
    kind: TaskDeadlineKind,
}

impl TimerEntry {
    pub(super) const fn new(
        deadline: MonotonicDeadline,
        thread: ThreadId,
        token: TaskDeadlineToken,
        kind: TaskDeadlineKind,
    ) -> Self {
        Self {
            deadline,
            thread,
            token,
            kind,
        }
    }

    pub(super) const fn deadline(self) -> MonotonicDeadline {
        self.deadline
    }

    pub(super) const fn thread(self) -> ThreadId {
        self.thread
    }

    pub(super) const fn token(self) -> TaskDeadlineToken {
        self.token
    }

    pub(super) const fn kind(self) -> TaskDeadlineKind {
        self.kind
    }

    pub(super) fn precedes(self, other: Self) -> bool {
        self.deadline < other.deadline
            || (self.deadline == other.deadline
                && (self.thread.as_u64() < other.thread.as_u64()
                    || (self.thread == other.thread
                        && (self.token.node().as_u64() < other.token.node().as_u64()
                            || (self.token.node() == other.token.node()
                                && self.token.generation() < other.token.generation())))))
    }
}

#[derive(Debug)]
pub(super) struct TimerHeap {
    entries: Vec<TimerEntry>,
    capacity: usize,
}

impl TimerHeap {
    pub(super) fn new(capacity: usize) -> Self {
        Self {
            entries: Vec::with_capacity(capacity),
            capacity,
        }
    }

    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(super) fn is_full(&self) -> bool {
        self.entries.len() == self.capacity
    }

    pub(super) fn contains_node(&self, node: TaskDeadlineNodeId) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.token().node() == node)
    }

    pub(super) fn peek(&self) -> Option<TimerEntry> {
        self.entries.first().copied()
    }

    pub(super) fn push(&mut self, entry: TimerEntry) {
        assert!(
            !self.is_full(),
            "fixed task deadline heap capacity invariant violated"
        );
        self.entries.push(entry);
        self.sift_up(self.entries.len() - 1);
    }

    pub(super) fn pop_min(&mut self) -> Option<TimerEntry> {
        let last = self.entries.pop()?;
        if self.entries.is_empty() {
            return Some(last);
        }
        let minimum = core::mem::replace(&mut self.entries[0], last);
        self.sift_down(0);
        Some(minimum)
    }

    pub(super) fn remove(
        &mut self,
        thread: ThreadId,
        token: TaskDeadlineToken,
        kind: TaskDeadlineKind,
    ) -> Option<TimerEntry> {
        let index = self.entries.iter().position(|entry| {
            entry.thread() == thread && entry.token() == token && entry.kind() == kind
        })?;
        let removed = self.entries.swap_remove(index);
        if index < self.entries.len() {
            if index > 0 {
                let parent = (index - 1) / 2;
                if self.entries[index].precedes(self.entries[parent]) {
                    self.sift_up(index);
                    return Some(removed);
                }
            }
            self.sift_down(index);
        }
        Some(removed)
    }

    pub(super) fn remove_node(&mut self, node: TaskDeadlineNodeId) -> Option<TimerEntry> {
        let index = self
            .entries
            .iter()
            .position(|entry| entry.token().node() == node)?;
        let removed = self.entries.swap_remove(index);
        if index < self.entries.len() {
            if index > 0 {
                let parent = (index - 1) / 2;
                if self.entries[index].precedes(self.entries[parent]) {
                    self.sift_up(index);
                    return Some(removed);
                }
            }
            self.sift_down(index);
        }
        Some(removed)
    }

    fn sift_up(&mut self, mut index: usize) {
        while index > 0 {
            let parent = (index - 1) / 2;
            if !self.entries[index].precedes(self.entries[parent]) {
                break;
            }
            self.entries.swap(index, parent);
            index = parent;
        }
    }

    fn sift_down(&mut self, mut index: usize) {
        loop {
            let left = index * 2 + 1;
            if left >= self.entries.len() {
                return;
            }
            let right = left + 1;
            let child =
                if right < self.entries.len() && self.entries[right].precedes(self.entries[left]) {
                    right
                } else {
                    left
                };
            if !self.entries[child].precedes(self.entries[index]) {
                return;
            }
            self.entries.swap(index, child);
            index = child;
        }
    }
}
