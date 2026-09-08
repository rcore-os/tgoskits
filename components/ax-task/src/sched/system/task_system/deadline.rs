//! Deadline diagnostics, deferred callbacks, and owner timer service.

use super::*;
use crate::{
    runtime::cpu::SchedulerDeadlineUpdate,
    sched::algorithm::{SchedulerClockEvent, scheduler_clock_event, scheduler_time_reached},
    time::queue::{KernelTimerAction, KernelTimerEntry},
};

enum OwnerDeadlineTimerPlan {
    Unchanged,
    Cancel,
    Arm(TaskDeadlineArmPlan),
}

#[derive(Clone, Copy)]
struct OwnerDeadlineDue {
    scheduler_now_ns: u64,
    cbs_expired: bool,
    zero_lag_reached: bool,
}

struct OwnerDeadlineReconcile<'a> {
    core: &'a Arc<ThreadCore>,
    sched: &'a mut ThreadSchedState,
    cpu: Pin<&'a mut CpuLocal>,
    due: OwnerDeadlineDue,
}

pub(crate) struct KtimerServiceBatch {
    processed: usize,
    pending: bool,
    update: Option<SchedulerDeadlineUpdate>,
    kernel_timer: Option<KernelTimerExecution>,
    task_timer: Option<ExpiredTaskDeadline>,
    completed_timer: Option<KernelTimerEntry>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct HardTimerServiceBatch {
    processed: usize,
    soft: SoftTimerExpireBatch,
}

pub(crate) struct KernelTimerCompletionBatch {
    completed: Option<KernelTimerEntry>,
    update: Option<SchedulerDeadlineUpdate>,
}

impl KernelTimerCompletionBatch {
    pub(crate) const fn update(&self) -> Option<SchedulerDeadlineUpdate> {
        self.update
    }

    pub(crate) fn take_completed(&mut self) -> Option<KernelTimerEntry> {
        self.completed.take()
    }
}

impl KtimerServiceBatch {
    pub(crate) const fn processed(&self) -> usize {
        self.processed
    }

    pub(crate) const fn pending(&self) -> bool {
        self.pending
    }

    pub(crate) const fn update(&self) -> Option<SchedulerDeadlineUpdate> {
        self.update
    }

    pub(crate) fn take_kernel_timer(&mut self) -> Option<KernelTimerExecution> {
        self.kernel_timer.take()
    }

    pub(crate) fn take_task_timer(&mut self) -> Option<ExpiredTaskDeadline> {
        self.task_timer.take()
    }

    pub(crate) fn take_completed_timer(&mut self) -> Option<KernelTimerEntry> {
        self.completed_timer.take()
    }
}

impl HardTimerServiceBatch {
    pub(crate) const fn processed(self) -> usize {
        self.processed
    }

    pub(crate) const fn soft(self) -> SoftTimerExpireBatch {
        self.soft
    }
}

mod overrun;

mod registration;

mod observation;

mod soft_timer;

mod expiry;

mod clockevent;
