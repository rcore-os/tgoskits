//! Generation-checked registry and scheduling orchestration.

mod balance;
mod cpu_lifecycle;
mod deadline;
mod deferred_work;
mod delivery;
mod dispatch;
mod exited_work;
mod lifecycle;
mod membarrier;
mod model;
mod outcome;
mod park_exit;
mod pi;
mod placement;
mod priority_index;
mod registry;
mod root_domain;
mod scheduling;
mod switch;
mod thread_api;
mod thread_callbacks;
mod thread_creation;

use alloc::{sync::Arc, vec::Vec};
use core::{pin::Pin, ptr};

use exited_work::ExitedThreadWork;
pub(crate) use membarrier::{MembarrierCpuTargets, MembarrierTarget};
use model::{
    BalanceReason, BalanceTransferOutcome, DeferredTaskWorkClass, DetachedOwnerMessageBatch,
    FAIR_BALANCE_BALANCED_BACKOFF_FACTOR, FAIR_BALANCE_CONSTRAINED_BACKOFF_FACTOR,
    FairBalanceResult, FairPolicyPlacement,
};
pub use model::{DeferredTaskWorkBatch, OwnedThreadReapError, TaskSystem};
pub(crate) use outcome::SwitchEndpoint;
pub use outcome::{
    ChargeOutcome, DeadlineActivitySnapshot, DeadlineRuntimeSnapshot, OwnerControlDrain,
    ScheduleDecision, SchedulerOutcome, SwitchInCompletion,
};
pub(crate) use park_exit::CurrentExitPermit;
use priority_index::RootDomainPriorityIndex;
use registry::{
    CpuRegistration, DeadlineCallbackClaim, DetachedThreadRecord, TaskSystemState, ThreadRecord,
    ThreadSlot,
};
use root_domain::{DeadlineBandwidthRebuild, RootDomain, RootDomainPushClass, RootDomainState};
use thread_callbacks::ThreadCallbackState;

use super::thread_sched::{
    PiScheduleUpdate, ThreadDeadlineInit, ThreadPlacementInit, ThreadPolicyInit, ThreadRuntimeInit,
    ThreadSchedCell, ThreadSchedInit, ThreadSchedState,
};
#[cfg(feature = "qperf-metrics")]
use crate::system::cpu::WakePreemptionDecision;
use crate::{
    ActiveSchedulingState, CpuId, CpuLocal, CpuRemote, CpuRemotePublication, CpuSet, CpuSnapshot,
    DEADLINE_CLASS_RANK, DeadlineAdmission, DeadlineBandwidthSnapshot, DeadlineEntity,
    DeadlineServer, EnqueueReason, FairMode, OwnedThreadSchedulerExit, ParkCommit, ParkPrepare,
    ParkTicket, PiDonation, PiMutexRaw, PiWaitKey, PiWaitRegistration, PiWaitToken, PickedThread,
    QueuedThread, QueuedThreadSnapshot, REALTIME_CLASS_RANK, RqTaskMetadata, SchedulePolicy,
    SchedulerDeadlineDerivationSource, SchedulerTimestamp, SchedulingClass, SchedulingEntity,
    SchedulingUrgency, SwitchReason, TaskError, TaskSystemConfig, ThreadAffinityChange, ThreadCore,
    ThreadExtension, ThreadExtensionBorrow, ThreadExtensionLease, ThreadExtensionView,
    ThreadHandle, ThreadId, ThreadResources, ThreadRuntimeSnapshot, ThreadSchedulerActivity,
    ThreadSpec, ThreadState, ThreadWakeBatch, ThreadWakeHandle, WaitWakeClaim, WaitWakeDelivery,
    WakeIntent, WakeResult,
    executor::CoroutineHeader,
    inbox::{InboxKind, InboxMessage, InboxOperation, PublishResult, SchedulerInbox},
    lock::{IrqScope, IrqTicketLock, PreemptTicketLock},
    runtime::{
        AddressSpaceDestroyOutcome, AddressSpaceMembarrierId, AddressSpaceMembarrierState,
        AddressSpaceReclaimArmOutcome, ContextThreadBinding, CpuRemoteHandle,
        CurrentThreadPublication, CurrentThreadRef, MembarrierRegistration,
        MembarrierRegistrationPhase, MonotonicDeadline, MonotonicInstant, RuntimeCpuId,
        RuntimeStatus, task_runtime,
    },
    system::cpu::{
        CpuRunQueueState, CurrentClassState, CurrentDispatch, CurrentDispatchState,
        DeadlineBaseGuardSource, EqualRtWakeAction, HardTimerServiceClaim, IdlePullReservation,
        KtimerServiceClaim, OwnerRqEntry, OwnerRqTxn, PreparedMigrationDelivery,
        PreparedRemoteWakeDelivery, RescheduleKind, RqTaskTime, RunQueueClockSnapshot,
        RunQueueGuardSource, SchedulerDeadlineRqObservation, SchedulerRequestScope,
    },
    task_work::{TaskWorkConsumerGuard, TaskWorkDoorbell},
    timer::{
        ExpiredTaskDeadline, KernelTimerExecution, TaskDeadlineArmPlan, TaskDeadlineError,
        TaskDeadlineKind, TaskDeadlineNode, TaskDeadlineQueue, TaskDeadlineRegistration,
    },
};

struct UnpublishedThreadGuard<'system> {
    system: &'system TaskSystem,
    spec: Option<ThreadSpec>,
}

fn apply_pi_schedule_update(
    sched: &mut ThreadSchedState,
    mut active: ActiveSchedulingState,
    update: PiScheduleUpdate,
    owner_now_ns: u64,
    fair_placement: Option<FairPolicyPlacement>,
) -> Result<ActiveSchedulingState, TaskError> {
    if update.generation != sched.policy.dispatch_generation {
        return Err(TaskError::InvalidPiState);
    }

    let PiScheduleUpdate {
        policy,
        donor,
        deadline_donor,
        deadline_donor_core,
        deadline_donor_server,
        generation: _,
    } = update;
    let old_donor = sched.pi.donor;
    let old_deadline_donor = sched.pi.deadline_donor;
    let base = sched.policy.base;
    let old_uses_inherited = active.uses_inherited_entity();
    let next_uses_inherited = donor.is_some() && !pi_reuses_base_entity(base, policy);
    if old_uses_inherited && !next_uses_inherited {
        active.use_base_entity(base);
    }

    let source_changed = old_donor != donor || old_deadline_donor != deadline_donor;
    let donor_server = deadline_donor_server;
    match (donor, policy) {
        (None, base_policy) if base_policy == base => {
            active.use_base_entity(base_policy);
        }
        (Some(_), SchedulePolicy::Deadline(_)) => {
            if !next_uses_inherited {
                return Err(TaskError::InvalidPiState);
            }
            if !old_uses_inherited || source_changed {
                active.use_inherited_entity(
                    policy,
                    SchedulingEntity::Deadline(crate::DeadlineEntity::from_donor_server(
                        sched.deadline.server.clone(),
                        donor_server.ok_or(TaskError::InvalidPiState)?,
                    )),
                );
            } else {
                active.update_inherited_effective_policy(policy);
            }
            let SchedulingEntity::Deadline(deadline) = active.entity() else {
                return Err(TaskError::InvalidPiState);
            };
            deadline.replenish_for_pi(owner_now_ns);
        }
        (Some(_), SchedulePolicy::Fifo { .. }) => {
            if next_uses_inherited {
                active.use_inherited_entity(policy, SchedulingEntity::Fifo);
            } else if !matches!(active.base_entity(), SchedulingEntity::Fifo) {
                return Err(TaskError::InvalidPiState);
            } else {
                active.use_base_entity_with_effective_policy(policy);
            }
        }
        (Some(_), SchedulePolicy::RoundRobin { quantum_ns, .. }) => {
            if next_uses_inherited {
                return Err(TaskError::InvalidPiState);
            }
            if !matches!(active.base_entity(), SchedulingEntity::RoundRobin { .. }) {
                return Err(TaskError::InvalidPiState);
            }
            if old_donor.is_none()
                && let SchedulingEntity::RoundRobin {
                    remaining_quantum_ns,
                } = active.base_entity_mut()
                && *remaining_quantum_ns > quantum_ns
            {
                *remaining_quantum_ns = quantum_ns;
            }
            active.use_base_entity_with_effective_policy(policy);
        }
        (Some(_), SchedulePolicy::Fair { nice, mode }) => {
            if next_uses_inherited {
                return Err(TaskError::InvalidPiState);
            }
            let SchedulingEntity::Fair(fair) = *active.base_entity() else {
                return Err(TaskError::InvalidPiState);
            };
            let placement = fair_placement.ok_or(TaskError::InvalidPiState)?;
            active.replace_base_entity(SchedulingEntity::Fair(fair.reconfigure(
                nice,
                mode,
                placement.source_virtual_time,
                placement.destination_virtual_time,
            )));
            active.use_base_entity_with_effective_policy(policy);
        }
        _ => return Err(TaskError::InvalidPiState),
    }

    sched.pi.donor = donor;
    sched.pi.deadline_donor = deadline_donor;
    sched.pi.deadline_donor_core = deadline_donor_core;
    Ok(active)
}

fn pi_reuses_base_entity(base: SchedulePolicy, effective: SchedulePolicy) -> bool {
    matches!(
        (base, effective),
        (
            SchedulePolicy::Fifo { .. } | SchedulePolicy::RoundRobin { .. },
            SchedulePolicy::Fifo { .. } | SchedulePolicy::RoundRobin { .. }
        ) | (SchedulePolicy::Fair { .. }, SchedulePolicy::Fair { .. })
    )
}

struct OwnerNext {
    core: Arc<ThreadCore>,
}

impl<'system> UnpublishedThreadGuard<'system> {
    fn new(system: &'system TaskSystem, spec: ThreadSpec) -> Self {
        Self {
            system,
            spec: Some(spec),
        }
    }

    fn into_owned_parts(mut self) -> (Option<ThreadExtension>, ThreadResources) {
        self.spec
            .take()
            .expect("unpublished thread transaction must still own its specification")
            .into_owned_parts()
    }

    fn into_spec(mut self) -> ThreadSpec {
        self.spec
            .take()
            .expect("unpublished thread transaction must still own its specification")
    }

    fn spec(&self) -> &ThreadSpec {
        self.spec
            .as_ref()
            .expect("unpublished thread transaction must still own its specification")
    }
}

impl Drop for UnpublishedThreadGuard<'_> {
    fn drop(&mut self) {
        if let Some(spec) = self.spec.take() {
            let (extension, resources) = spec.into_owned_parts();
            self.system
                .release_unpublished_thread(DetachedThreadRecord::new(resources, extension));
        }
    }
}

impl TaskSystem {
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
            .map(|index| CpuRemote::create(CpuId::new(index as u32), config))
            .collect::<Vec<_>>();
        let cpu_registrations = cpu_remotes
            .iter()
            .cloned()
            .map(|remote| CpuRegistration { remote })
            .collect();
        let root_domain = RootDomain::new(config, cpu_remotes.clone());
        Ok(Self {
            config,
            cpu_remotes,
            state: PreemptTicketLock::new(TaskSystemState {
                cpus: cpu_registrations,
                slots: Vec::new(),
                free_slots: Vec::new(),
                pending_address_space_reclaims: Vec::new(),
                task_work_class_cursor: DeferredTaskWorkClass::Deadline,
                address_space_reclaim_first: true,
                exited_work: ExitedThreadWork::new(),
            }),
            root_domain,
            deferred_coroutine_reclaims: SchedulerInbox::new(InboxKind::Reclaim),
            deferred_deadline_callbacks: SchedulerInbox::new(InboxKind::TaskWork),
            deferred_scheduler_ticks: SchedulerInbox::new(InboxKind::TaskWork),
            task_work,
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
        || config.thread_capacity() == 0
        || config.thread_capacity() > u32::MAX as usize
        || config.batch_limit() == 0
        || config.batch_limit() > crate::DEFAULT_BATCH_LIMIT
        || config.pi_chain_limit() == 0
    {
        return Err(TaskError::InvalidConfiguration);
    }
    Ok(())
}

fn deadline_zero_lag(deadline: &DeadlineEntity) -> SchedulerTimestamp {
    let policy = deadline.policy();
    let lag_ns = deadline.remaining_runtime_ns() as u128 * policy.period_ns() as u128
        / policy.runtime_ns() as u128;
    let lag_ns = u64::try_from(lag_ns)
        .expect("Deadline zero-lag interval cannot exceed one scheduler period");
    SchedulerTimestamp::from_nanos(
        deadline
            .absolute_deadline_ns()
            .expect("an active Deadline entity must own a zero-lag anchor"),
    )
    .retreat(lag_ns)
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
    assert_eq!(
        slot.pending_deadline_reservation, 0,
        "a reusable thread slot must not retain Deadline admission"
    );
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
