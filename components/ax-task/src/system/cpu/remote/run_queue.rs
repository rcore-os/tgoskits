use core::ops::{Deref, DerefMut};

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WakePreemptionDecision {
    KeepCurrent,
    WakeeSelected,
    QueuedCandidateSelected,
}

impl WakePreemptionDecision {
    pub(crate) const fn requests_reschedule(self) -> bool {
        matches!(self, Self::WakeeSelected)
    }
}

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

    /// Applies Linux EEVDF wakeup preemption to the complete owner runqueue.
    ///
    /// A fair wakee may request rescheduling only when it both defeats the
    /// protected current request and is itself the earliest eligible queued
    /// entity. Comparing only the wakee with current creates needless
    /// reschedule IPIs when an older queued contender would be selected.
    pub(crate) fn wakee_preemption(
        &self,
        wakee: ThreadId,
        policy: SchedulePolicy,
        entity: SchedulingEntity,
        fair_virtual_time: u64,
    ) -> WakePreemptionDecision {
        let Some(current) = self.current else {
            return WakePreemptionDecision::WakeeSelected;
        };
        if !current.should_preempt(policy, entity, fair_virtual_time) {
            return WakePreemptionDecision::KeepCurrent;
        }
        match policy {
            SchedulePolicy::Fair { mode, .. } => {
                if self
                    .queue
                    .fair_wakee_is_selected(wakee, mode, fair_virtual_time)
                {
                    WakePreemptionDecision::WakeeSelected
                } else {
                    WakePreemptionDecision::QueuedCandidateSelected
                }
            }
            _ => WakePreemptionDecision::WakeeSelected,
        }
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
