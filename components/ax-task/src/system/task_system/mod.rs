//! Generation-checked registry and scheduling orchestration.

mod balance;
mod cpu_lifecycle;
mod deadline;
mod deferred_work;
mod delivery;
mod dispatch;
mod lifecycle;
mod model;
mod outcome;
mod park_exit;
mod pi;
mod placement;
mod registry;
mod scheduling;
mod switch;
mod thread_api;
mod thread_creation;

use alloc::{sync::Arc, vec::Vec};
use core::{
    pin::Pin,
    ptr,
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
};

use model::{
    BalanceReason, DeferredTaskWorkClass, DetachedOwnerMessageBatch, DetachedPayloadKind,
    FAIR_BALANCE_BALANCED_BACKOFF_FACTOR, FAIR_BALANCE_CONSTRAINED_BACKOFF_FACTOR,
    FairBalanceResult, FairPolicyPlacement, RootDomainState,
};
pub use model::{DeferredTaskWorkBatch, OwnedThreadReapError, TaskSystem};
pub(crate) use outcome::SwitchEndpoint;
pub use outcome::{
    ChargeOutcome, DeadlineActivitySnapshot, DeadlineRuntimeSnapshot, RemoteWakeDrain,
    ScheduleDecision, SchedulerOutcome,
};
pub use pi::{PiMutexClaim, PiMutexHandoff, PiMutexRelease};
use registry::{
    CpuRegistration, DetachedThreadRecord, PendingResourceRelease, PiRecomputeProof,
    PiWaitRegistration, TaskSystemState, ThreadRecord, ThreadSlot,
};

use super::thread_sched::{
    DeadlineActivity, SchedulerPlacement, ThreadSchedCell, ThreadSchedState,
};
#[cfg(test)]
use crate::runtime::ExecutionContextHandle;
use crate::{
    CpuId, CpuLocal, CpuRemote, CpuRemotePublication, CpuSet, CpuSnapshot, DeadlineAdmission,
    DeadlineBandwidthSnapshot, DeadlineEntity, DetachedQueueEntry, EnqueueReason, FairMode,
    ParkCommit, ParkPrepare, ParkTicket, PiLockId, PiWaitToken, QueuedThread, SchedulePolicy,
    SchedulingClass, SchedulingEntity, SwitchReason, TaskError, TaskSystemConfig,
    ThreadAffinityChange, ThreadCore, ThreadExtension, ThreadExtensionBorrow, ThreadExtensionLease,
    ThreadExtensionView, ThreadHandle, ThreadId, ThreadLifecycle, ThreadResources,
    ThreadRuntimeSnapshot, ThreadSpec, ThreadState,
    inbox::{InboxKind, InboxMessage, InboxOperation, PublishResult, SchedulerInbox},
    lock::{IrqScope, PreemptTicketLock, SequenceCounter},
    reclaim::DeferredReclaimNode,
    runtime::{
        ContextThreadBinding, CpuRemoteHandle, RuntimeCpuId, RuntimeStatus, ThreadIdentityV1,
        task_runtime,
    },
    system::cpu::{CurrentDispatch, CurrentDispatchState, IdlePullReservation},
    task_work::{TaskWorkConsumerGuard, TaskWorkDoorbell},
    timer::{
        ExpiredTaskDeadline, TaskDeadlineError, TaskDeadlineKind, TaskDeadlineNode,
        TaskDeadlineRegistration,
    },
};

struct UnpublishedThreadGuard<'system> {
    system: &'system TaskSystem,
    record: Option<DetachedThreadRecord>,
}

struct OwnerNext {
    core: Arc<ThreadCore>,
    outgoing_migration_target: Option<CpuId>,
}

impl<'system> UnpublishedThreadGuard<'system> {
    fn new(system: &'system TaskSystem, spec: ThreadSpec) -> Self {
        let (extension, resources) = spec.into_owned_parts();
        Self {
            system,
            record: Some(DetachedThreadRecord::new(resources, extension)),
        }
    }

    fn into_owned_parts(mut self) -> (Option<ThreadExtension>, ThreadResources) {
        self.record
            .take()
            .expect("unpublished thread transaction must still own its record")
            .into_owned_parts()
    }
}

impl Drop for UnpublishedThreadGuard<'_> {
    fn drop(&mut self) {
        if let Some(record) = self.record.take() {
            let _release = self.system.release_unpublished_thread(record);
        }
    }
}

impl TaskSystem {
    fn drain_pending_deadline_admission(&self, state: &mut TaskSystemState) {
        let released = self
            .pending_deadline_admission_release
            .swap(0, Ordering::AcqRel);
        state.deadline_admission.release(u128::from(released));
    }

    fn defer_deadline_admission_release(&self, released: u64) -> Result<(), TaskError> {
        self.pending_deadline_admission_release
            .try_update(Ordering::AcqRel, Ordering::Acquire, |pending| {
                pending.checked_add(released)
            })
            .map(|_| ())
            .map_err(|_| TaskError::InvalidConfiguration)
    }

    /// Creates an empty scheduler instance for a fixed topology.
    ///
    /// # Errors
    ///
    /// Returns [`TaskError::InvalidCpuCount`] for an empty or unrepresentable
    /// topology and [`TaskError::InvalidConfiguration`] for inconsistent fixed
    /// capacities or bandwidth values.
    pub fn new(config: TaskSystemConfig) -> Result<Self, TaskError> {
        validate_config(config)?;
        let task_work = Arc::new(TaskWorkDoorbell::new());
        let cpu_remotes = (0..config.cpu_count())
            .map(|index| CpuRemote::create(CpuId::new(index as u32)))
            .collect::<Vec<_>>();
        let cpu_registrations = cpu_remotes
            .iter()
            .cloned()
            .map(|remote| CpuRegistration { remote })
            .collect();
        Ok(Self {
            config,
            cpu_remotes,
            state: PreemptTicketLock::new(TaskSystemState {
                cpus: cpu_registrations,
                slots: Vec::new(),
                free_slots: Vec::new(),
                pending_resource_releases: Vec::new(),
                task_work_class_cursor: DeferredTaskWorkClass::Deadline,
                thread_release_first: true,
                deadline_callback_cursor: 0,
                exit_callback_cursor: 0,
                reap_cursor: 0,
                deadline_admission: DeadlineAdmission::new(config.deadline_cap_percent()),
            }),
            root_domain: PreemptTicketLock::new(RootDomainState {
                online: CpuSet::empty(config.cpu_count()),
            }),
            deferred_reclaims: SchedulerInbox::new(InboxKind::Reclaim),
            deferred_scheduler_ticks: SchedulerInbox::new(InboxKind::TaskWork),
            task_work,
            topology_sequence: SequenceCounter::default(),
            online_count: AtomicUsize::new(0),
            pending_deadline_admission_release: AtomicU64::new(0),
        })
    }
}

fn validate_config(config: TaskSystemConfig) -> Result<(), TaskError> {
    if config.cpu_count() == 0 || config.cpu_count() > u32::MAX as usize {
        return Err(TaskError::InvalidCpuCount(config.cpu_count()));
    }
    if config.deadline_cap_percent() == 0
        || config.deadline_cap_percent() > 100
        || config.rt_period_ns() == 0
        || config.rt_runtime_ns() > config.rt_period_ns()
        || config.balance_interval_ns() == 0
        || config.timer_capacity() == 0
        || config.batch_limit() == 0
        || config.batch_limit() > crate::DEFAULT_BATCH_LIMIT
    {
        return Err(TaskError::InvalidConfiguration);
    }
    Ok(())
}

fn deadline_zero_lag_ns(deadline: DeadlineEntity) -> u64 {
    let policy = deadline.policy();
    let lag_ns = (deadline.remaining_runtime_ns() as u128)
        .saturating_mul(policy.period_ns() as u128)
        / policy.runtime_ns() as u128;
    deadline
        .absolute_deadline_ns()
        .saturating_sub(u64::try_from(lag_ns).unwrap_or(u64::MAX))
}

fn ensure_runtime_success(status: RuntimeStatus) -> Result<(), TaskError> {
    if status == RuntimeStatus::Success {
        Ok(())
    } else {
        Err(TaskError::RuntimeFailure(status as u32))
    }
}

fn validate_affinity(affinity: &CpuSet, cpu_count: usize) -> Result<(), TaskError> {
    if affinity.topology_len() == cpu_count {
        Ok(())
    } else {
        Err(TaskError::InvalidConfiguration)
    }
}

// The top bit of the generation-bearing identity is reserved for compact
// scheduler-adjacent owner words such as the Linux-style PI mutex waiters bit.
// Exhausting a slot retires it instead of wrapping and reintroducing ABA.
const MAX_THREAD_GENERATION: u32 = i32::MAX as u32;

const fn next_generation(generation: u32) -> u32 {
    if generation < MAX_THREAD_GENERATION {
        generation + 1
    } else {
        generation
    }
}

fn advance_thread_slot_generation(slot: &mut ThreadSlot) -> bool {
    let next = next_generation(slot.generation);
    if next == slot.generation {
        // The empty slot remains in the registry so every stale identity still
        // resolves to `record == None`, but it is never returned to free_slots:
        // wrapping would make an older generation-bearing ThreadId valid again.
        false
    } else {
        slot.generation = next;
        true
    }
}

#[cfg(test)]
mod tests;
