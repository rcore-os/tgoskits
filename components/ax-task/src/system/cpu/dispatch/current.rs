//! `rq->curr` identity and scheduler-class ownership.

use core::ptr::NonNull;

use super::super::*;
use crate::{LinkedRqTaskRef, system::task_system::SwitchEndpoint};

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
    remote_publication: CurrentRemotePublication,
}

/// Stable task identity and runtime resources pinned by `rq->curr`.
#[derive(Debug)]
pub(super) struct CurrentTaskIdentity {
    thread: ThreadId,
    runtime_core: SchedulerThreadRef,
    runtime_owner: Option<Arc<ThreadCore>>,
    owned_metadata: Option<RqTaskMetadata>,
    linked: Option<LinkedRqTaskRef>,
}

/// Stable Linux-style `rq->curr` pointer whose lifetime is owned separately.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SchedulerThreadRef(NonNull<ThreadCore>);

/// Stable reference to the selected task's effective policy record.
///
/// The policy lives in the same boxed scheduling record that is retained by
/// rq current or its linked RT/Deadline node. It therefore follows the same
/// switch-tail lifetime as [`SchedulerThreadRef`] without copying the widest
/// policy variant through each scheduler result.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SchedulerPolicyRef(NonNull<SchedulePolicy>);

// SAFETY: the pointed-to core is Arc-allocated. While the reference is live,
// an owned current dispatch or a linked RT/DL rq node retains that same Arc.
// All ownership transitions happen under the owner rq lock.
unsafe impl Send for SchedulerThreadRef {}
unsafe impl Sync for SchedulerThreadRef {}

// SAFETY: SchedulePolicy is Send + Sync, and the scheduler-owned box that pins
// this pointer cannot be moved out or mutated before switch-tail consumption.
unsafe impl Send for SchedulerPolicyRef {}
unsafe impl Sync for SchedulerPolicyRef {}

impl SchedulerThreadRef {
    fn from_ref(core: &ThreadCore) -> Self {
        Self(NonNull::from(core))
    }

    /// Captures a non-owning reference pinned by scheduler state.
    ///
    /// # Safety
    ///
    /// The caller must have installed `core` as `rq->curr` and must keep the
    /// current dispatch or its linked RT/DL node alive until the returned
    /// reference is consumed by the same CPU's context-switch tail.
    pub(crate) unsafe fn from_scheduler_owned(core: &ThreadCore) -> Self {
        Self::from_ref(core)
    }

    pub(crate) fn as_ref(&self) -> &ThreadCore {
        // SAFETY: construction and every ownership transition preserve the
        // lifetime invariant documented on `SchedulerThreadRef`.
        unsafe { self.0.as_ref() }
    }

    fn clone_arc(&self) -> Arc<ThreadCore> {
        let pointer = self.0.as_ptr();
        // SAFETY: every live scheduler reference is pinned by either the
        // current dispatch's Arc or the current RT/DL rq node's Arc.
        unsafe {
            Arc::increment_strong_count(pointer);
            Arc::from_raw(pointer)
        }
    }
}

impl SchedulerPolicyRef {
    /// Captures a non-owning policy reference pinned by scheduler state.
    ///
    /// # Safety
    ///
    /// The active scheduling record containing `policy` must remain owned by
    /// rq current or a linked rq node until the context-switch tail consumes
    /// this reference, and the policy must not be updated during that span.
    pub(crate) unsafe fn from_scheduler_owned(policy: &SchedulePolicy) -> Self {
        Self(NonNull::from(policy))
    }

    pub(crate) fn get(self) -> SchedulePolicy {
        // SAFETY: construction transfers the scheduler-owned lifetime proof.
        unsafe { *self.0.as_ref() }
    }
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
}

/// Load-placement facts contributed by the running task.
///
/// These values change only at policy, affinity, or idle-role transitions.
/// Keeping them with `rq->curr` avoids re-deriving static policy facts during
/// every context switch merely to prove that remote placement state is stable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CurrentRemotePublication(u64);

impl CurrentRemotePublication {
    const IDLE: Self = Self(0);

    pub(crate) fn task(policy: SchedulePolicy, metadata: &RqTaskMetadata) -> Self {
        let fair_demand = policy.fair_demand();
        let rt_wake_donor = policy
            .rt_priority()
            .map(|priority| {
                u16::from(priority.get())
                    | if metadata.affinity.is_migration_capable() {
                        1 << 7
                    } else {
                        0
                    }
            })
            .unwrap_or(0);
        let fixed_demand = matches!(
            policy,
            SchedulePolicy::Deadline(_)
                | SchedulePolicy::Fifo { .. }
                | SchedulePolicy::RoundRobin { .. }
        );
        let idle_fair = matches!(
            policy,
            SchedulePolicy::Fair {
                mode: FairMode::Idle,
                ..
            }
        );
        Self(
            fair_demand
                | (u64::from(rt_wake_donor) << 32)
                | (u64::from(fixed_demand) << 40)
                | (u64::from(idle_fair) << 41),
        )
    }
}

/// Class-state ownership during one dispatch interval.
#[derive(Debug)]
pub(crate) enum CurrentClassState {
    /// Fair/stop current owns the entity removed from its class structure.
    Owned(ActiveSchedulingState),
    /// RT/Deadline current remains owned by its active rq node.
    Linked,
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
        *self.schedule_policy_ref()
    }

    pub(crate) fn schedule_policy_ref(&self) -> &SchedulePolicy {
        match self.schedule() {
            CurrentClassState::Owned(active) => active.policy_ref(),
            CurrentClassState::Linked => self
                .task
                .linked
                .as_ref()
                .expect("linked class state must retain its rq node")
                .thread()
                .active
                .policy_ref(),
        }
    }

    pub(crate) fn owned_scheduling_entity_ref(&self) -> Option<&SchedulingEntity> {
        match self.schedule() {
            CurrentClassState::Owned(active) => Some(active.entity()),
            CurrentClassState::Linked => None,
        }
    }

    pub(crate) const fn is_linked(&self) -> bool {
        matches!(self.class.schedule, Some(CurrentClassState::Linked))
    }

    pub(crate) fn linked_task_ref(&self) -> Option<LinkedRqTaskRef> {
        self.is_linked().then(|| {
            self.task
                .linked
                .expect("linked class state must retain its rq node")
        })
    }

    pub(crate) fn owned_base_scheduling_entity_ref(&self) -> Option<&SchedulingEntity> {
        match self.schedule() {
            CurrentClassState::Owned(active) => Some(active.base_entity()),
            CurrentClassState::Linked => None,
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
            CurrentClassState::Linked => {
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
            CurrentClassState::Linked => None,
        }
    }

    /// Transfers the runtime-core pin together with rq-owned class state.
    ///
    /// A linked RT/Deadline current has no detached class entity, but the same
    /// `rq->curr` record still owns the runtime-core reference needed by the
    /// switch handoff. Consuming both from one record avoids reacquiring that
    /// reference while the owner rq lock is held.
    pub(crate) fn into_runtime_core_and_active(
        self,
    ) -> (Arc<ThreadCore>, Option<ActiveSchedulingState>) {
        let Self {
            task,
            class,
            accounting: _,
            remote_publication: _,
        } = self;
        let active = match class
            .schedule
            .expect("rq transaction must reinstall current class state")
        {
            CurrentClassState::Owned(active) => Some(active),
            CurrentClassState::Linked => None,
        };
        let runtime_core = task
            .runtime_owner
            .expect("unlinked current must retain its runtime core");
        (runtime_core, active)
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
            remote_publication: _,
        } = self;
        if class.role != DispatchRole::Task {
            return None;
        }
        let active = match class
            .schedule
            .expect("rq transaction must reinstall current class state")
        {
            CurrentClassState::Owned(active) => active,
            CurrentClassState::Linked => return None,
        };
        let runtime_core = task
            .runtime_owner
            .expect("unlinked current must own its runtime core");
        let metadata = task
            .owned_metadata
            .expect("unlinked current must own its task resources");
        let migration_capable = metadata.affinity.is_migration_capable();
        Some(QueuedThread::new(
            task.thread,
            active,
            runtime_core,
            class.rt_quota_exempt,
            migration_capable,
            metadata,
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
            linked @ CurrentClassState::Linked => {
                self.class.schedule = Some(linked);
                None
            }
        }
    }

    pub(crate) fn install_reclassified_owned(
        &mut self,
        active: ActiveSchedulingState,
        runtime_core: Arc<ThreadCore>,
        metadata: RqTaskMetadata,
        rt_quota_exempt: bool,
    ) {
        if matches!(self.class.schedule, Some(CurrentClassState::Owned(_))) {
            panic!("owned current must transfer its entity before reclassification");
        }
        if self.thread() != runtime_core.id() {
            panic!("reclassification must preserve current runtime identity");
        }
        self.task.runtime_core = SchedulerThreadRef::from_ref(&runtime_core);
        self.task.runtime_owner = Some(runtime_core);
        self.task.owned_metadata = Some(metadata);
        self.task.linked = None;
        self.class.schedule = Some(CurrentClassState::Owned(active));
        self.class.rt_quota_exempt = rt_quota_exempt;
        self.refresh_remote_publication();
    }

    pub(crate) fn install_reclassified_linked(&mut self, linked: LinkedRqTaskRef) {
        let thread = linked.thread();
        if self.thread() != thread.id {
            panic!("reclassification must preserve current runtime identity");
        }
        self.task.runtime_core = SchedulerThreadRef::from_ref(&thread.core);
        self.task.runtime_owner = None;
        self.task.owned_metadata = None;
        self.task.linked = Some(linked);
        self.class.schedule = Some(CurrentClassState::Linked);
        self.class.rt_quota_exempt = thread.rt_quota_exempt;
        self.remote_publication = thread.remote_publication;
    }

    /// Pins task resources before the canonical linked node is removed.
    pub(crate) fn retain_task_before_unlink(
        &mut self,
        runtime_core: Arc<ThreadCore>,
        metadata: RqTaskMetadata,
    ) {
        if self.thread() != runtime_core.id()
            || self.task.linked.is_none()
            || self.task.owned_metadata.is_some()
        {
            panic!("unlink must preserve one linked current identity");
        }
        if self.task.runtime_owner.replace(runtime_core).is_some() {
            panic!("linked current must not replace an existing runtime owner");
        }
        self.task.owned_metadata = Some(metadata);
        self.task.linked = None;
    }

    /// Refreshes task-control metadata after an in-place class transition.
    pub(crate) fn update_affinity(&mut self, affinity: Arc<CpuSet>) {
        let metadata = self
            .task
            .owned_metadata
            .as_mut()
            .expect("linked current affinity is owned by its rq node");
        metadata.affinity = affinity;
        self.refresh_remote_publication();
    }

    pub(crate) fn metadata(&self) -> &RqTaskMetadata {
        if let Some(linked) = self.task.linked.as_ref() {
            &linked.thread().metadata
        } else {
            self.task
                .owned_metadata
                .as_ref()
                .expect("unlinked current must own its task metadata")
        }
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

    #[inline(always)]
    pub(crate) fn owned(
        runtime_core: Arc<ThreadCore>,
        active: ActiveSchedulingState,
        metadata: RqTaskMetadata,
        rt_quota_exempt: bool,
        now: RqTaskTime,
    ) -> Self {
        let now_ns = now.as_nanos();
        let remote_publication = CurrentRemotePublication::task(active.policy(), &metadata);
        Self {
            task: CurrentTaskIdentity {
                thread: runtime_core.id(),
                runtime_core: SchedulerThreadRef::from_ref(&runtime_core),
                runtime_owner: Some(runtime_core),
                owned_metadata: Some(metadata),
                linked: None,
            },
            class: CurrentClassDispatch {
                schedule: Some(CurrentClassState::Owned(active)),
                rt_quota_exempt,
                deadline_overrun: false,
                role: DispatchRole::Task,
            },
            accounting: CurrentRuntimeAccounting {
                accounted_until_ns: now_ns,
            },
            remote_publication,
        }
    }

    #[inline(always)]
    pub(crate) fn linked(linked: LinkedRqTaskRef, now: RqTaskTime) -> Self {
        let thread = linked.thread();
        Self {
            task: CurrentTaskIdentity {
                thread: thread.id,
                runtime_core: SchedulerThreadRef::from_ref(&thread.core),
                runtime_owner: None,
                owned_metadata: None,
                linked: Some(linked),
            },
            class: CurrentClassDispatch {
                schedule: Some(CurrentClassState::Linked),
                rt_quota_exempt: thread.rt_quota_exempt,
                deadline_overrun: false,
                role: DispatchRole::Task,
            },
            accounting: CurrentRuntimeAccounting {
                accounted_until_ns: now.as_nanos(),
            },
            remote_publication: thread.remote_publication,
        }
    }

    /// Advances Linux-style `rq->curr` to another stable rq-linked node.
    #[inline(always)]
    pub(crate) fn replace_linked(&mut self, linked: LinkedRqTaskRef, now: RqTaskTime) {
        let thread = linked.thread();
        self.task.thread = thread.id;
        self.task.runtime_core = SchedulerThreadRef::from_ref(&thread.core);
        self.task.linked = Some(linked);
        self.class.rt_quota_exempt = thread.rt_quota_exempt;
        self.class.deadline_overrun = false;
        self.accounting.accounted_until_ns = now.as_nanos();
        self.remote_publication = thread.remote_publication;
    }

    pub(crate) fn switch_endpoint(&self) -> SwitchEndpoint {
        SwitchEndpoint::new(
            self.thread(),
            self.metadata().runtime_binding,
            self.runtime_core().membarrier_identity(),
        )
    }

    pub(crate) fn address_space(&self) -> crate::runtime::AddressSpaceHandle {
        self.metadata().runtime_binding.address_space()
    }

    pub(crate) fn update_runtime_binding(&mut self, binding: crate::runtime::ThreadRuntimeBinding) {
        let metadata = self
            .task
            .owned_metadata
            .as_mut()
            .expect("linked current runtime binding is owned by its rq node");
        metadata.runtime_binding = binding;
    }

    pub(crate) fn with_role(mut self, role: DispatchRole) -> Self {
        self.class.role = role;
        self.refresh_remote_publication();
        self
    }

    pub(crate) const fn remote_publication(&self) -> CurrentRemotePublication {
        self.remote_publication
    }

    fn refresh_remote_publication(&mut self) {
        self.remote_publication = if self.is_dedicated_idle() {
            CurrentRemotePublication::IDLE
        } else {
            CurrentRemotePublication::task(self.schedule_policy(), self.metadata())
        };
    }

    pub(crate) const fn rt_quota_exempt(&self) -> bool {
        self.class.rt_quota_exempt
    }

    pub(crate) fn runtime_core(&self) -> &ThreadCore {
        self.task.runtime_core.as_ref()
    }

    pub(crate) fn clone_runtime_core(&self) -> Arc<ThreadCore> {
        self.task.runtime_core.clone_arc()
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
            // Linux SCHED_RR rotates from the periodic scheduler tick. Its
            // quantum never contributes an independent hrtick deadline.
            SchedulingEntity::RoundRobin { .. } => None,
            SchedulingEntity::Deadline(deadline) => Some(deadline.remaining_runtime_ns()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_robin_quantum_does_not_arm_a_runtime_clockevent() {
        assert_eq!(
            CurrentDispatch::runtime_timer_delta_for(
                &SchedulingEntity::RoundRobin {
                    remaining_quantum_ns: 30,
                },
                0,
            ),
            None,
        );
    }
}
