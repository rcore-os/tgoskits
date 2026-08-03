use core::ops::{Deref, DerefMut};

use super::*;

/// Scheduler state protected by the target CPU's irqsave runqueue lock.
///
/// Mutable runtime accounting and switch-tail state remain owner-only in
/// [`CpuLocal`]. The current scheduling snapshot is committed here with
/// physical queue membership so a remote waker can evaluate preemption.
#[derive(Debug)]
pub(crate) struct CpuRunQueueState {
    queue: RunQueue,
    current: Option<CurrentSchedule>,
}

impl CpuRunQueueState {
    pub(crate) fn new() -> Self {
        Self {
            queue: RunQueue::new(),
            current: None,
        }
    }

    pub(crate) const fn current(&self) -> Option<CurrentSchedule> {
        self.current
    }

    pub(crate) fn set_current(&mut self, current: Option<CurrentSchedule>) {
        self.current = current;
    }
}

impl Deref for CpuRunQueueState {
    type Target = RunQueue;

    fn deref(&self) -> &Self::Target {
        &self.queue
    }
}

impl DerefMut for CpuRunQueueState {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.queue
    }
}
