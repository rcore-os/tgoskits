//! Per-thread scheduler state independent from the generation registry.

mod deadline_state;
mod pi_state;
mod placement;
mod policy_state;
mod runtime_state;

use alloc::sync::{Arc, Weak};

pub(in crate::system) use pi_state::PiScheduleUpdate;
pub(in crate::system) use placement::SchedulerPlacement;
#[cfg(all(axtest, feature = "axtest"))]
pub(crate) use placement::exercise_on_cpu_publication_kinds;
#[cfg(feature = "task-test-hooks")]
pub(crate) use placement::take_on_cpu_waits;

use crate::{
    ActiveSchedulingState, CpuId, CpuSet, DeadlineServer, DetachedActiveGuard,
    DetachedActivePublication, DetachedActiveState, SchedulePolicy, SchedulerTimestamp,
    SchedulingEntity, TaskError, ThreadCore, ThreadId, ThreadLifecycle, ThreadState,
    lock::{IrqTicketGuard, IrqTicketLock},
    runtime::{AddressSpaceHandle, ExecutionContextHandle},
    timer::TaskDeadlineRegistration,
};

/// GRUB activity of one admitted Deadline reservation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeadlineActivity {
    /// Ready or executing, and therefore contributing active utilization.
    ActiveContending,
    /// Blocked before zero-lag while still contributing active utilization.
    ActiveNonContending,
    /// Blocked past zero-lag and eligible to donate inactive utilization.
    Inactive,
}

/// Stable scheduler ownership anchor retained by every runnable reference.
///
/// Owner CPUs operate on this cell through queued, current, and inbox-held
/// `ThreadCore` references. Registry locking is reserved for lifecycle lookup,
/// admission, and PI graph changes rather than owner runqueue progress.
#[derive(Debug)]
pub(crate) struct ThreadSchedCell {
    id: ThreadId,
    lifecycle: alloc::sync::Arc<ThreadLifecycle>,
    placement: alloc::sync::Arc<placement::SchedulerPlacement>,
    deadline_server: DeadlineServer,
    detached_active: DetachedActiveState,
    state: IrqTicketLock<ThreadSchedState>,
}

impl ThreadSchedCell {
    pub(super) fn new(id: ThreadId, init: ThreadSchedInit) -> Self {
        let (state, active) = ThreadSchedState::new(init);
        let lifecycle = alloc::sync::Arc::clone(&state.lifecycle);
        let placement = alloc::sync::Arc::clone(&state.placement);
        let deadline_server = state.deadline.server.clone();
        Self {
            id,
            lifecycle,
            placement,
            deadline_server,
            detached_active: DetachedActiveState::new(active),
            state: IrqTicketLock::new(state),
        }
    }

    pub(crate) const fn id(&self) -> ThreadId {
        self.id
    }

    pub(super) fn lock(&self) -> IrqTicketGuard<'_, ThreadSchedState> {
        loop {
            let guard = self
                .state
                .lock(crate::runtime::IrqGuardSource::ThreadSchedTicket);
            if !self.detached_active.publication_in_progress() {
                return guard;
            }
            #[cfg(feature = "task-test-hooks")]
            crate::task_test_hooks::record_thread_sched_publication_wait(self.id);
            // The rq owner publishing a detached entity never needs this task
            // lock. Release it before waiting so one delayed CPU cannot turn
            // the move-only entity handoff into a task-wide lock stall. After
            // the Acquire wait, retry the lock and every protected predicate.
            drop(guard);
            self.detached_active.wait_for_publication();
        }
    }

    #[cfg(feature = "task-test-hooks")]
    pub(crate) fn try_lock_state_for_test(&self) -> bool {
        self.state
            .try_lock(crate::runtime::IrqGuardSource::ThreadSchedTicket)
            .is_some()
    }

    #[cfg(feature = "task-test-hooks")]
    pub(crate) fn hold_lock_for_test(&self) {
        let _guard = self.lock();
        crate::task_test_hooks::pause_thread_sched_lock_hold(self.id);
    }

    /// Locks scheduler state below the runtime's IRQ-off scheduler baton.
    ///
    /// # Safety
    ///
    /// The scheduler frame must remain active until the returned guard is
    /// dropped. Ordinary task context must use [`Self::lock`].
    pub(super) unsafe fn lock_scheduler_frame(&self) -> IrqTicketGuard<'_, ThreadSchedState> {
        loop {
            // SAFETY: forwarded from this method's scheduler-baton contract.
            let guard = unsafe { self.state.lock_irq_disabled() };
            if !self.detached_active.publication_in_progress() {
                return guard;
            }
            #[cfg(feature = "task-test-hooks")]
            crate::task_test_hooks::record_thread_sched_publication_wait(self.id);
            drop(guard);
            self.detached_active.wait_for_publication();
        }
    }

    /// Tries to lock task state while its owner rq lock is active.
    ///
    /// This is the sole inverse-order acquisition used to finish Linux Fair
    /// delayed dequeue. It never waits: a concurrent `p->pi_lock -> rq` owner
    /// wins and the rq picker skips that delayed entity for this pass.
    ///
    /// # Safety
    ///
    /// The owner rq guard must keep local IRQs disabled for the complete
    /// returned-guard lifetime.
    pub(super) unsafe fn try_lock_from_owner_rq(
        &self,
    ) -> Option<IrqTicketGuard<'_, ThreadSchedState>> {
        // SAFETY: forwarded from this method's owner-rq/IRQ-off contract.
        let guard = unsafe { self.state.try_lock_irq_disabled() }?;
        if self.detached_active.publication_in_progress() {
            drop(guard);
            return None;
        }
        Some(guard)
    }

    /// Locks scheduler state during offline CPU bootstrap.
    ///
    /// # Safety
    ///
    /// The caller must retain raw local IRQ exclusion and the boot CPU's
    /// `PREEMPT_DISABLED` ownership for the complete guard lifetime.
    pub(super) unsafe fn lock_bootstrap(&self) -> IrqTicketGuard<'_, ThreadSchedState> {
        loop {
            // SAFETY: forwarded from this method's offline boot-owner contract.
            let guard = unsafe { self.state.lock_irq_disabled() };
            if !self.detached_active.publication_in_progress() {
                return guard;
            }
            #[cfg(feature = "task-test-hooks")]
            crate::task_test_hooks::record_thread_sched_publication_wait(self.id);
            drop(guard);
            self.detached_active.wait_for_publication();
        }
    }

    /// Borrows the off-rq entity under this task's scheduler lock.
    pub(super) fn active(&self, _sched: &ThreadSchedState) -> DetachedActiveGuard<'_> {
        self.detached_active.active()
    }

    /// Borrows the off-rq entity when task placement says it may be detached.
    pub(super) fn active_option(
        &self,
        _sched: &ThreadSchedState,
    ) -> Option<DetachedActiveGuard<'_>> {
        self.detached_active.active_option()
    }

    /// Moves off-rq entity ownership into an rq/current representation.
    pub(super) fn take_active(&self, _sched: &mut ThreadSchedState) -> ActiveSchedulingState {
        self.detached_active
            .take()
            .expect("active scheduling state must have exactly one owner")
    }

    /// Returns rq/current entity ownership to this task's stable slot.
    pub(super) fn install_active(
        &self,
        _sched: &mut ThreadSchedState,
        active: ActiveSchedulingState,
    ) {
        self.detached_active.install(active);
    }

    /// Reserves detached ownership for the rq-only FIFO/RR block path.
    pub(super) fn begin_active_publication(&self) -> Option<DetachedActivePublication<'_>> {
        self.detached_active.begin_publication()
    }

    #[cfg(any(test, all(axtest, feature = "axtest")))]
    pub(super) fn owns_active(&self) -> bool {
        self.detached_active.owns_active()
    }

    pub(crate) fn scheduler_fence_cpu(&self) -> Option<CpuId> {
        self.placement.on_cpu()
    }

    pub(crate) fn assigned_cpu(&self) -> Option<CpuId> {
        self.placement.assigned_cpu()
    }

    #[cfg(feature = "task-test-hooks")]
    pub(crate) fn retains_delayed_fair_membership_for_test(&self) -> bool {
        self.lifecycle.state() == ThreadState::Blocked && self.placement.queued_cpu().is_some()
    }

    #[cfg(feature = "task-test-hooks")]
    pub(crate) fn affinity_settlement_generation_for_test(&self, cpu: CpuId) -> Option<u64> {
        let state = self.lock();
        (state.affinity.affinity.count() == 1
            && state.affinity.affinity.contains(cpu)
            && !self.placement.has_pending_migration()
            && [self.placement.queued_cpu(), self.placement.on_cpu()]
                .into_iter()
                .flatten()
                .all(|placed| placed == cpu))
        .then_some(state.affinity.affinity_generation)
    }

    #[cfg(feature = "task-test-hooks")]
    pub(crate) fn has_committed_migration_to_cpu_for_test(&self, cpu: CpuId) -> bool {
        self.placement.committed_migration_target() == Some(cpu)
    }

    pub(in crate::system) fn placement(&self) -> &placement::SchedulerPlacement {
        self.placement.as_ref()
    }

    pub(crate) fn lifecycle(&self) -> &alloc::sync::Arc<ThreadLifecycle> {
        &self.lifecycle
    }

    pub(crate) fn deadline_server(&self) -> DeadlineServer {
        self.deadline_server.clone()
    }

    #[cfg(any(test, all(axtest, feature = "axtest")))]
    pub(crate) fn new_test(id: ThreadId, policy: SchedulePolicy) -> Self {
        let deadline_server = DeadlineServer::unbound();
        let entity =
            SchedulingEntity::new_with_deadline_server(policy, 1, 0, deadline_server.clone());
        Self::new(
            id,
            ThreadSchedInit {
                policy: ThreadPolicyInit { policy, entity },
                placement: ThreadPlacementInit {
                    initial_cpu: CpuId::new(0),
                    affinity: CpuSet::all(1),
                },
                deadline: ThreadDeadlineInit {
                    server: deadline_server,
                    reservation_scaled: 0,
                },
                runtime: ThreadRuntimeInit {
                    context: ExecutionContextHandle::NONE,
                    address_space: AddressSpaceHandle::NONE,
                },
            },
        )
    }
}

#[derive(Debug)]
pub(super) struct ThreadSchedState {
    pub(super) lifecycle: alloc::sync::Arc<ThreadLifecycle>,
    pub(super) policy: policy_state::ThreadPolicyState,
    pub(super) placement: alloc::sync::Arc<placement::SchedulerPlacement>,
    pub(super) affinity: placement::ThreadAffinityState,
    pub(super) deadline: deadline_state::ThreadDeadlineState,
    pub(super) pi: pi_state::ThreadPiState,
    pub(super) runtime: runtime_state::ThreadRuntimeState,
}

pub(super) struct ThreadPolicyInit {
    pub(super) policy: SchedulePolicy,
    pub(super) entity: SchedulingEntity,
}

pub(super) struct ThreadPlacementInit {
    pub(super) initial_cpu: CpuId,
    pub(super) affinity: CpuSet,
}

pub(super) struct ThreadDeadlineInit {
    pub(super) server: DeadlineServer,
    pub(super) reservation_scaled: u64,
}

pub(super) struct ThreadRuntimeInit {
    pub(super) context: ExecutionContextHandle,
    pub(super) address_space: AddressSpaceHandle,
}

pub(super) struct ThreadSchedInit {
    pub(super) policy: ThreadPolicyInit,
    pub(super) placement: ThreadPlacementInit,
    pub(super) deadline: ThreadDeadlineInit,
    pub(super) runtime: ThreadRuntimeInit,
}

impl ThreadSchedState {
    pub(super) fn new(init: ThreadSchedInit) -> (Self, ActiveSchedulingState) {
        let active = ActiveSchedulingState::new(init.policy.policy, init.policy.entity);
        (
            Self {
                lifecycle: alloc::sync::Arc::new(ThreadLifecycle::new()),
                policy: policy_state::ThreadPolicyState::new(init.policy.policy),
                placement: alloc::sync::Arc::new(placement::SchedulerPlacement::new(
                    init.placement.initial_cpu,
                )),
                affinity: placement::ThreadAffinityState::new(init.placement.affinity),
                deadline: deadline_state::ThreadDeadlineState::new(
                    init.deadline.server,
                    init.deadline.reservation_scaled,
                ),
                pi: pi_state::ThreadPiState::new(),
                runtime: runtime_state::ThreadRuntimeState::new(
                    init.runtime.context,
                    init.runtime.address_space,
                ),
            },
            active,
        )
    }

    pub(super) fn transition(
        &mut self,
        core: &ThreadCore,
        state: ThreadState,
    ) -> Result<(), TaskError> {
        core.transition_state(state)
    }

    pub(super) fn is_pi_boosted_rt_owner_for(&self, policy: SchedulePolicy) -> bool {
        !self.pi.donors.is_empty()
            && self.is_pi_boosted()
            && matches!(
                policy,
                SchedulePolicy::Fifo { .. } | SchedulePolicy::RoundRobin { .. }
            )
    }

    pub(super) const fn is_pi_boosted(&self) -> bool {
        self.pi.donor.is_some()
    }

    /// Returns the root-domain reservation retained by the applied policy or
    /// by the one not-yet-applied owner transaction.
    ///
    /// Admission accounts the maximum, not the sum: publishing a replacement
    /// transaction transfers one reservation to another without admitting two
    /// Deadline entities for the same task.
    pub(super) fn held_deadline_reservation(&self) -> u64 {
        self.deadline.bandwidth.reservation_scaled().max(
            self.policy
                .pending_update()
                .map_or(0, |pending| pending.reservation_scaled),
        )
    }

    /// Builds the task-control snapshot published with an rq entity.
    ///
    /// Callers hold this task's scheduler lock and then acquire the owner rq,
    /// matching Linux's `p->pi_lock` to rq publication order.
    pub(super) fn rq_task_metadata(&self) -> Result<crate::scheduler::RqTaskMetadata, TaskError> {
        Ok(crate::scheduler::RqTaskMetadata {
            affinity: Arc::clone(&self.affinity.affinity),
            deadline_bandwidth_scaled: self.deadline.bandwidth.reservation_scaled(),
            runtime_binding: self.runtime.binding(),
        })
    }
}
