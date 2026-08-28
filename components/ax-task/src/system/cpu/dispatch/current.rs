//! `rq->curr` identity and scheduler-class ownership.

use super::super::*;
use crate::system::task_system::SwitchEndpoint;

/// Owner-CPU copy of the running thread's mutable dispatch state.
///
/// The enclosing [`CpuRunQueueState`] is the sole owner. Fair/stop current
/// entities live here while absent from their class tree; RT/Deadline current
/// entities remain linked in the corresponding active class structure.
#[derive(Debug)]
pub(crate) struct CurrentDispatch {
    pub(super) task: CurrentTaskIdentity,
    pub(super) class: CurrentClassDispatch,
    pub(super) accounting: CurrentRuntimeAccounting,
}

/// Stable task identity and runtime resources pinned by `rq->curr`.
#[derive(Debug)]
pub(super) struct CurrentTaskIdentity {
    pub(super) thread: ThreadId,
    pub(super) runtime_core: Arc<ThreadCore>,
    metadata: RqTaskMetadata,
}

/// Scheduler-class state owned by the current runqueue interval.
#[derive(Debug)]
pub(super) struct CurrentClassDispatch {
    pub(super) schedule: Option<CurrentClassState>,
    rt_quota_exempt: bool,
    pub(super) deadline_overrun: bool,
    role: DispatchRole,
}

/// Runtime accounting sampled only from `rq->clock_task`.
#[derive(Debug)]
pub(super) struct CurrentRuntimeAccounting {
    pub(super) accounted_until_ns: u64,
    pub(super) charged_runtime_ns: u64,
}

/// Registry state copied into one owner-CPU dispatch interval.
#[derive(Debug)]
pub(crate) struct CurrentDispatchState {
    pub(crate) thread: ThreadId,
    pub(crate) schedule: CurrentClassState,
    pub(crate) metadata: RqTaskMetadata,
    pub(crate) rt_quota_exempt: bool,
}

/// Class-state ownership during one dispatch interval.
#[derive(Debug)]
pub(crate) enum CurrentClassState {
    /// Fair/stop current owns the entity removed from its class structure.
    Owned(ActiveSchedulingState),
    /// RT/Deadline current remains owned by its active rq node. Dispatch keeps
    /// only class metadata; the entity itself never leaves the rq node.
    Linked { policy: SchedulePolicy },
}

/// Runqueue role of the current dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DispatchRole {
    Task,
    DedicatedIdle,
}

impl CurrentDispatch {
    fn schedule(&self) -> &CurrentClassState {
        self.class
            .schedule
            .as_ref()
            .expect("rq transaction must reinstall current class state")
    }

    pub(crate) const fn thread(&self) -> ThreadId {
        self.task.thread
    }

    pub(crate) fn schedule_policy(&self) -> SchedulePolicy {
        match self.schedule() {
            CurrentClassState::Owned(active) => active.policy(),
            CurrentClassState::Linked { policy } => *policy,
        }
    }

    pub(crate) fn owned_scheduling_entity_ref(&self) -> Option<&SchedulingEntity> {
        match self.schedule() {
            CurrentClassState::Owned(active) => Some(active.entity()),
            CurrentClassState::Linked { .. } => None,
        }
    }

    pub(crate) fn owned_base_scheduling_entity_ref(&self) -> Option<&SchedulingEntity> {
        match self.schedule() {
            CurrentClassState::Owned(active) => Some(active.base_entity()),
            CurrentClassState::Linked { .. } => None,
        }
    }

    pub(crate) fn active_mut(&mut self) -> &mut ActiveSchedulingState {
        match self
            .class
            .schedule
            .as_mut()
            .expect("rq transaction must reinstall current class state")
        {
            CurrentClassState::Owned(active) => active,
            CurrentClassState::Linked { .. } => {
                panic!("linked current entity is owned by its rq class node")
            }
        }
    }

    pub(crate) fn into_active(self) -> Option<ActiveSchedulingState> {
        match self
            .class
            .schedule
            .expect("rq transaction must reinstall current class state")
        {
            CurrentClassState::Owned(active) => Some(active),
            CurrentClassState::Linked { .. } => None,
        }
    }

    /// Converts an unlinked current dispatch into one rq-owned queued task.
    ///
    /// The complete task-control snapshot travels with the active entity, so
    /// ordinary `put_prev_task()` never has to reopen the task scheduler lock.
    pub(crate) fn into_queued_thread(self) -> Option<QueuedThread> {
        let Self {
            task,
            class,
            accounting: _,
        } = self;
        if class.role != DispatchRole::Task {
            return None;
        }
        let active = match class
            .schedule
            .expect("rq transaction must reinstall current class state")
        {
            CurrentClassState::Owned(active) => active,
            CurrentClassState::Linked { .. } => return None,
        };
        let migration_capable = task.metadata.affinity.is_migration_capable();
        Some(QueuedThread::new(
            task.thread,
            active,
            task.runtime_core,
            class.rt_quota_exempt,
            migration_capable,
            task.metadata,
        ))
    }

    pub(crate) fn take_owned_for_reclassify(&mut self) -> Option<ActiveSchedulingState> {
        match self
            .class
            .schedule
            .take()
            .expect("rq transaction must own current class state")
        {
            CurrentClassState::Owned(active) => Some(active),
            linked @ CurrentClassState::Linked { .. } => {
                self.class.schedule = Some(linked);
                None
            }
        }
    }

    pub(crate) fn install_reclassified_schedule(&mut self, schedule: CurrentClassState) {
        if matches!(self.class.schedule, Some(CurrentClassState::Owned(_))) {
            panic!("owned current must transfer its entity before reclassification");
        }
        self.class.schedule = Some(schedule);
    }

    /// Refreshes task-control metadata after an in-place class transition.
    ///
    /// The caller holds the task PI lock and owner rq lock. Keeping this copy
    /// in the same transaction prevents timer IRQ accounting from observing a
    /// new class entity with stale PI donor or Deadline rescue state.
    pub(crate) fn refresh_scheduler_metadata(
        &mut self,
        metadata: RqTaskMetadata,
        rt_quota_exempt: bool,
    ) {
        self.task.metadata = metadata;
        self.class.rt_quota_exempt = rt_quota_exempt;
    }

    pub(crate) fn update_affinity(&mut self, affinity: Arc<CpuSet>) {
        self.task.metadata.affinity = affinity;
    }

    pub(crate) const fn metadata(&self) -> &RqTaskMetadata {
        &self.task.metadata
    }

    pub(crate) fn placement_demand(&self) -> u64 {
        if matches!(self.class.role, DispatchRole::DedicatedIdle) {
            0
        } else {
            self.schedule_policy().placement_demand()
        }
    }

    pub(crate) const fn is_dedicated_idle(&self) -> bool {
        matches!(self.class.role, DispatchRole::DedicatedIdle)
    }

    pub(crate) fn fair_demand(&self) -> u64 {
        if matches!(self.class.role, DispatchRole::DedicatedIdle) {
            0
        } else {
            self.schedule_policy().fair_demand()
        }
    }

    pub(crate) fn new(
        state: CurrentDispatchState,
        runtime_core: &Arc<ThreadCore>,
        now: RqTaskTime,
    ) -> Self {
        let now_ns = now.as_nanos();
        Self {
            task: CurrentTaskIdentity {
                thread: state.thread,
                runtime_core: Arc::clone(runtime_core),
                metadata: state.metadata,
            },
            class: CurrentClassDispatch {
                schedule: Some(state.schedule),
                rt_quota_exempt: state.rt_quota_exempt,
                deadline_overrun: false,
                role: DispatchRole::Task,
            },
            accounting: CurrentRuntimeAccounting {
                accounted_until_ns: now_ns,
                charged_runtime_ns: 0,
            },
        }
    }

    pub(crate) fn switch_endpoint(&self) -> SwitchEndpoint {
        SwitchEndpoint::new(
            self.task.thread,
            self.task.metadata.runtime_binding,
            self.task.runtime_core.extension_view(),
        )
    }

    pub(crate) const fn address_space(&self) -> crate::runtime::AddressSpaceHandle {
        self.task.metadata.runtime_binding.address_space()
    }

    pub(crate) fn update_runtime_binding(&mut self, binding: crate::runtime::ThreadRuntimeBinding) {
        self.task.metadata.runtime_binding = binding;
    }

    pub(crate) const fn with_role(mut self, role: DispatchRole) -> Self {
        self.class.role = role;
        self
    }

    pub(crate) const fn rt_quota_exempt(&self) -> bool {
        self.class.rt_quota_exempt
    }

    pub(crate) fn runtime_core(&self) -> &ThreadCore {
        &self.task.runtime_core
    }

    pub(crate) fn runtime_core_arc(&self) -> &Arc<ThreadCore> {
        &self.task.runtime_core
    }

    pub(crate) fn deadline_overrun_core(&self) -> Arc<ThreadCore> {
        Arc::clone(&self.task.runtime_core)
    }

    pub(crate) fn is_rt(&self) -> bool {
        matches!(
            self.schedule_policy(),
            SchedulePolicy::Fifo { .. } | SchedulePolicy::RoundRobin { .. }
        )
    }

    /// Returns the remaining task-clock budget for the running class.
    ///
    /// Absolute Deadline release/deadline events live in `rq->clock` and are
    /// retained by the linked DL entity. They must never be compared with this
    /// `rq->clock_task` duration.
    pub(crate) fn runtime_timer_delta_for(
        entity: &SchedulingEntity,
        irq_util_avg: u32,
    ) -> Option<u64> {
        match entity {
            SchedulingEntity::KernelStop => None,
            SchedulingEntity::Fair(fair) => {
                Some(fair.finish_runtime_deadline_delta_ns(irq_util_avg))
            }
            SchedulingEntity::Fifo => None,
            SchedulingEntity::RoundRobin {
                remaining_quantum_ns,
            } => Some(*remaining_quantum_ns),
            SchedulingEntity::Deadline(deadline) => Some(deadline.remaining_runtime_ns()),
        }
    }
}
