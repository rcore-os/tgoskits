//! Allocation-once binary min-heap for pinned timer pointers.

use alloc::vec::Vec;

use super::{TimerNode, TimerToken};

#[derive(Clone, Copy, Debug)]
pub(super) struct TimerEntry {
    deadline_ns: u64,
    token: TimerToken,
    node: *const TimerNode,
    owner: usize,
    owner_class: u64,
}

impl TimerEntry {
    pub(super) const fn new(
        deadline_ns: u64,
        token: TimerToken,
        node: *const TimerNode,
        owner: usize,
        owner_class: u64,
    ) -> Self {
        Self {
            deadline_ns,
            token,
            node,
            owner,
            owner_class,
        }
    }

    pub(super) const fn deadline_ns(self) -> u64 {
        self.deadline_ns
    }

    pub(super) const fn token(self) -> TimerToken {
        self.token
    }

    pub(super) const fn node(self) -> *const TimerNode {
        self.node
    }

    pub(super) const fn owner(self) -> usize {
        self.owner
    }

    pub(super) const fn owner_class(self) -> u64 {
        self.owner_class
    }

    const fn precedes(self, other: Self) -> bool {
        self.deadline_ns < other.deadline_ns
            || (self.deadline_ns == other.deadline_ns
                && self.token.generation() < other.token.generation())
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

    pub(super) const fn capacity(&self) -> usize {
        self.capacity
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

    pub(super) fn peek(&self) -> Option<TimerEntry> {
        self.entries.first().copied()
    }

    pub(super) fn push(&mut self, entry: TimerEntry) {
        debug_assert!(!self.is_full());
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
        node: *const TimerNode,
        token: TimerToken,
    ) -> Option<TimerEntry> {
        let index = self
            .entries
            .iter()
            .position(|entry| entry.node() == node && entry.token() == token)?;
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
