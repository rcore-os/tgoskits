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
    /// Deadline bandwidth and membership belong to the same transaction as
    /// physical runqueue membership. Remote wakeups therefore cannot expose a
    /// runnable Deadline entity before its CBS reservation is accounted.
    deadline_members: Vec<Arc<ThreadCore>>,
    deadline_admitted_bw_scaled: u64,
    deadline_running_bw_scaled: u64,
    deadline_max_bw_scaled: u64,
}

impl CpuRunQueueState {
    pub(crate) fn new(config: TaskSystemConfig) -> Self {
        Self {
            queue: RunQueue::new(),
            current: None,
            deadline_members: Vec::with_capacity(config.timer_capacity()),
            deadline_admitted_bw_scaled: 0,
            deadline_running_bw_scaled: 0,
            deadline_max_bw_scaled: u64::from(config.deadline_cap_percent()) * 10_000_000,
        }
    }

    pub(crate) const fn current(&self) -> Option<CurrentSchedule> {
        self.current
    }

    pub(crate) fn set_current(&mut self, current: Option<CurrentSchedule>) {
        self.current = current;
    }

    pub(crate) fn deadline_members_are_empty(&self) -> bool {
        self.deadline_members.is_empty()
    }

    pub(crate) fn register_deadline_member(
        &mut self,
        core: &Arc<ThreadCore>,
    ) -> Result<bool, TaskError> {
        if self
            .deadline_members
            .iter()
            .any(|member| Arc::ptr_eq(member, core))
        {
            return Ok(false);
        }
        if self.deadline_members.len() == self.deadline_members.capacity() {
            return Err(TaskError::TimerCapacity);
        }
        self.deadline_members.push(Arc::clone(core));
        Ok(true)
    }

    pub(crate) fn unregister_deadline_member(&mut self, core: &Arc<ThreadCore>) {
        if let Some(index) = self
            .deadline_members
            .iter()
            .position(|member| Arc::ptr_eq(member, core))
        {
            self.deadline_members.swap_remove(index);
        }
    }

    pub(crate) fn add_deadline_bandwidth(
        &mut self,
        utilization_scaled: u64,
        active: bool,
    ) -> Result<(), TaskError> {
        let admitted = self
            .deadline_admitted_bw_scaled
            .checked_add(utilization_scaled)
            .ok_or(TaskError::InvalidConfiguration)?;
        let running = if active {
            self.deadline_running_bw_scaled
                .checked_add(utilization_scaled)
                .ok_or(TaskError::InvalidConfiguration)?
        } else {
            self.deadline_running_bw_scaled
        };
        self.deadline_admitted_bw_scaled = admitted;
        self.deadline_running_bw_scaled = running;
        Ok(())
    }

    pub(crate) fn remove_deadline_bandwidth(
        &mut self,
        utilization_scaled: u64,
        active: bool,
    ) -> Result<(), TaskError> {
        let admitted = self
            .deadline_admitted_bw_scaled
            .checked_sub(utilization_scaled)
            .ok_or(TaskError::InvalidConfiguration)?;
        let running = if active {
            self.deadline_running_bw_scaled
                .checked_sub(utilization_scaled)
                .ok_or(TaskError::InvalidConfiguration)?
        } else {
            self.deadline_running_bw_scaled
        };
        self.deadline_admitted_bw_scaled = admitted;
        self.deadline_running_bw_scaled = running;
        Ok(())
    }

    pub(crate) fn activate_deadline_bandwidth(
        &mut self,
        utilization_scaled: u64,
    ) -> Result<(), TaskError> {
        let running = self
            .deadline_running_bw_scaled
            .checked_add(utilization_scaled)
            .ok_or(TaskError::InvalidConfiguration)?;
        if running > self.deadline_admitted_bw_scaled {
            return Err(TaskError::InvalidConfiguration);
        }
        self.deadline_running_bw_scaled = running;
        Ok(())
    }

    pub(crate) fn deactivate_deadline_bandwidth(
        &mut self,
        utilization_scaled: u64,
    ) -> Result<(), TaskError> {
        self.deadline_running_bw_scaled = self
            .deadline_running_bw_scaled
            .checked_sub(utilization_scaled)
            .ok_or(TaskError::InvalidConfiguration)?;
        Ok(())
    }

    pub(crate) const fn deadline_bandwidth(&self) -> DeadlineBandwidthSnapshot {
        DeadlineBandwidthSnapshot {
            this_bw_scaled: self.deadline_admitted_bw_scaled,
            running_bw_scaled: self.deadline_running_bw_scaled,
            max_bw_scaled: self.deadline_max_bw_scaled,
        }
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
