//! Generation-checked registry and scheduling orchestration.

mod balance;
mod cpu_lifecycle;
mod deadline;
mod deferred_work;
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
pub use pi::PiMutexHandoff;
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
    CpuId, CpuLocal, CpuRemote, CpuSet, CpuSnapshot, DeadlineAdmission, DeadlineBandwidthSnapshot,
    DeadlineEntity, EnqueueReason, FairMode, ParkCommit, ParkPrepare, ParkTicket, PiLockId,
    PiWaitToken, QueuedThread, SchedulePolicy, SchedulingClass, SchedulingEntity, SwitchReason,
    TaskError, TaskSystemConfig, ThreadCore, ThreadExtension, ThreadExtensionBorrow,
    ThreadExtensionLease, ThreadExtensionView, ThreadHandle, ThreadId, ThreadLifecycle,
    ThreadResources, ThreadRuntimeSnapshot, ThreadSpec, ThreadState, ThreadWakeHandle,
    inbox::{InboxKind, InboxMessage, InboxOperation, PublishResult, SchedulerInbox},
    lock::{IrqScope, IrqTicketLock, SequenceCounter},
    reclaim::DeferredReclaimNode,
    runtime::{ContextThreadBinding, RuntimeStatus, ThreadIdentityV1, task_runtime},
    system::cpu::{CurrentDispatch, CurrentDispatchState},
    task_work::{TaskWorkConsumerGuard, TaskWorkDoorbell},
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
            .map(|remote| CpuRegistration {
                online: false,
                remote,
            })
            .collect();
        Ok(Self {
            config,
            cpu_remotes,
            state: IrqTicketLock::new(TaskSystemState {
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
            root_domain: IrqTicketLock::new(RootDomainState {
                online: CpuSet::empty(config.cpu_count()),
            }),
            deferred_reclaims: SchedulerInbox::new(InboxKind::Reclaim),
            task_work,
            topology_sequence: SequenceCounter::default(),
            online_count: AtomicUsize::new(0),
            pending_deadline_admission_release: AtomicU64::new(0),
        })
    }

    /// Enqueues a ready thread on an affinity-compatible owner CPU.
    pub fn enqueue(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        thread: ThreadId,
        now_ns: u64,
    ) -> Result<(), TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        let core = {
            let state = self.state.lock();
            state.ensure_cpu_online(&cpu)?;
            Arc::clone(&state.thread_record(thread)?.core)
        };
        self.enqueue_owner_thread(cpu.as_mut(), core, now_ns, EnqueueReason::Wake)?;
        Self::program_local_timer(cpu.as_mut(), now_ns)
    }

    /// Places a newly ready thread on an allowed online CPU.
    ///
    /// If `cpu` is allowed, placement is a normal local enqueue. Otherwise the
    /// thread is transferred directly to the least-loaded allowed CPU through
    /// its owner-only migration inbox. This avoids ever publishing a pinned
    /// thread on a disallowed run queue while keeping [`Self::enqueue`] strict.
    ///
    /// # Errors
    ///
    /// Returns an error when the source CPU is offline, the thread is not a
    /// unique unqueued Ready thread, no allowed CPU is online, or local timer
    /// programming fails.
    pub fn place_ready(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        thread: ThreadId,
        now_ns: u64,
    ) -> Result<(), TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        let placed_locally = {
            let state = self.state.lock();
            state.ensure_cpu_online(&cpu)?;
            let owner = cpu.owner();
            let affinity = state.thread_record(thread)?.sched.lock().affinity.clone();
            if affinity.contains(owner) {
                let core = Arc::clone(&state.thread_record(thread)?.core);
                drop(state);
                self.enqueue_owner_thread(cpu.as_mut(), core, now_ns, EnqueueReason::Wake)?;
                true
            } else {
                let target = state
                    .select_allowed_cpu(&affinity)
                    .ok_or(TaskError::InvalidConfiguration)?;
                let core = {
                    let record = state.thread_record(thread)?;
                    let mut sched = record.sched.lock();
                    if sched.lifecycle.state() != ThreadState::Ready {
                        return Err(TaskError::NotReady);
                    }
                    if sched.placement.queued_cpu().is_some()
                        || sched.placement.running_cpu().is_some()
                        || sched.placement.on_cpu().is_some()
                    {
                        return Err(TaskError::AlreadyQueued);
                    }
                    sched.placement.set_migration_target(Some(target))?;
                    record.core.set_target_cpu(target);
                    Arc::clone(&record.core)
                };
                state.publish_migration_to(&core, target, owner, target)?;
                false
            }
        };
        if placed_locally {
            Self::program_local_timer(cpu.as_mut(), now_ns)
        } else {
            Ok(())
        }
    }

    /// Removes a ready thread from its owner run queue for migration or update.
    pub fn dequeue(&self, mut cpu: Pin<&mut CpuLocal>, thread: ThreadId) -> Result<(), TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        let state = self.state.lock();
        state.ensure_cpu_online(&cpu)?;
        let queued = cpu
            .as_mut()
            .fields_mut()
            .run_queue
            .dequeue(thread)
            .ok_or(TaskError::NotReady)?;
        let record = state.thread_record(thread)?;
        let mut sched = record.sched.lock();
        sched.entity = queued.entity;
        if !sched.is_pi_boosted() {
            sched.base_entity = queued.entity;
        }
        sched.placement.set_queued_cpu(None)?;
        drop(sched);
        drop(state);
        self.publish_owner_cpu_load_summary(cpu.as_mut());
        Ok(())
    }

    /// Drains a bounded batch of direct remote wakes on the owner CPU.
    pub fn drain_remote_wakes(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<RemoteWakeDrain, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        self.ensure_owner_cpu_online(&cpu)?;
        cpu.acknowledge_scheduler_ipi();
        let (drained, pending) = {
            let fields = cpu.as_mut().fields_mut();
            let limit = fields.batch_limit();
            let remote = Arc::clone(fields.remote());
            let buffer = &mut fields.remote_wake_buffer;
            let batch = remote.remote_wake_inbox().drain(limit, buffer);
            (batch.drained(), batch.pending())
        };
        let mut detached = [InboxMessage::EMPTY; crate::DEFAULT_BATCH_LIMIT];
        detached[..drained].copy_from_slice(&cpu.remote_wake_buffer[..drained]);
        let mut messages =
            DetachedOwnerMessageBatch::new(&detached[..drained], DetachedPayloadKind::RemoteWake);
        while let Some(message) = messages.next() {
            if message.payload() == 0 {
                continue;
            }
            // SAFETY: ThreadWakeHandle::wake transfers one Arc strong count in
            // every published non-zero payload. This owner drain consumes it
            // exactly once after the intrusive node was detached.
            let core = unsafe {
                Arc::from_raw(ptr::with_exposed_provenance::<ThreadCore>(
                    message.payload(),
                ))
            };
            if core.id() != message.thread_id() {
                continue;
            }
            if Self::consume_owner_wake(&core)? {
                let owner = cpu.owner();
                let target = core.target_cpu().unwrap_or(owner);
                if target == owner {
                    self.enqueue_owner_thread(cpu.as_mut(), core, now_ns, EnqueueReason::Wake)?;
                } else {
                    // Affinity may change after an IRQ publishes into the old
                    // target inbox. The old owner consumes the wake transition
                    // but hands the ready thread to the latest target instead
                    // of losing it on an affinity-invalid local enqueue.
                    Self::detach_owner_deadline_bandwidth(&core, cpu.as_mut())?;
                    core.sched()
                        .lock()
                        .placement
                        .set_migration_target(Some(target))?;
                    self.publish_owner_migration(&core, target, owner, target)?;
                }
            }
        }
        if pending {
            cpu.request_scheduler_work();
        }
        Ok(RemoteWakeDrain { drained, pending })
    }

    /// Reconciles task metadata written by a remote affinity setter with the
    /// physical placement owned by this CPU.
    ///
    /// The affinity mask may be updated under the stable thread lock from any
    /// CPU. Runqueue membership and switch-tail state are different: only the
    /// CPU named by [`SchedulerPlacement`] may mutate them. This is the local
    /// equivalent of Linux taking a task's `pi_lock` together with its owning
    /// runqueue lock before moving a queued task.
    fn reconcile_owner_affinity_update(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        core: &Arc<ThreadCore>,
    ) -> Result<(), TaskError> {
        let owner = cpu.owner();
        let mut sched = core.sched().lock();
        let running_cpu = sched.placement.running_cpu();
        let queued_cpu = sched.placement.queued_cpu();
        let on_cpu = sched.placement.on_cpu();
        let migration_target = sched.placement.migration_target();
        let physical_owner = running_cpu
            .or(queued_cpu)
            .or(on_cpu)
            .or(migration_target)
            .or(sched.deadline_bandwidth_cpu);
        let target = if sched.affinity.contains(owner) {
            owner
        } else {
            self.select_allowed_online_cpu(&sched.affinity, Some(owner))
                .ok_or(TaskError::InvalidConfiguration)?
        };
        core.set_target_cpu(target);

        if let Some(physical_owner) = physical_owner
            && physical_owner != owner
        {
            drop(sched);
            return self.publish_owner_affinity_retry(core, physical_owner, target);
        }

        // A switch handoff owns both the old stack and its committed
        // destination until switch tail clears `on_cpu`. Re-publish the
        // control request rather than rewriting that destination behind the
        // already staged handoff.
        if on_cpu == Some(owner) && running_cpu.is_none() {
            drop(sched);
            self.publish_owner_affinity_retry(core, owner, target)?;
            cpu.request_scheduler_work();
            return Ok(());
        }

        if queued_cpu == Some(owner) {
            if target == owner {
                sched.placement.set_migration_target(None)?;
                return Ok(());
            }
            let queued = cpu
                .as_mut()
                .fields_mut()
                .run_queue
                .dequeue(core.id())
                .ok_or(TaskError::NotReady)?;
            Self::detach_owner_deadline_bandwidth_locked(core, &mut sched, cpu.as_mut())?;
            sched.entity = queued.entity;
            if !sched.is_pi_boosted() {
                sched.base_entity = queued.entity;
            }
            sched.placement.set_migration_target(Some(target))?;
            sched.placement.set_queued_cpu(None)?;
            drop(sched);
            self.publish_owner_cpu_load_summary(cpu.as_mut());
            return self.publish_owner_migration(core, target, owner, target);
        }

        if running_cpu == Some(owner) {
            if cpu.current() != Some(core.id()) {
                return Err(TaskError::InvalidConfiguration);
            }
            sched
                .placement
                .set_migration_target((target != owner).then_some(target))?;
            drop(sched);
            if target != owner {
                cpu.request_reschedule();
            }
            self.publish_owner_cpu_load_summary(cpu.as_mut());
            return Ok(());
        }

        if migration_target == Some(owner) {
            if target != owner {
                sched.placement.set_migration_target(Some(target))?;
                drop(sched);
                return self.publish_owner_migration(core, target, owner, target);
            }
            return Ok(());
        }

        if sched.deadline_bandwidth_cpu == Some(owner) && target != owner {
            return Err(TaskError::InvalidConfiguration);
        }
        Ok(())
    }

    /// Applies a bounded batch of owner-CPU effective-policy updates.
    pub fn drain_policy_updates(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<RemoteWakeDrain, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        self.ensure_owner_cpu_online(&cpu)?;
        let (drained, pending) = {
            let fields = cpu.as_mut().fields_mut();
            let limit = fields.batch_limit();
            let remote = Arc::clone(fields.remote());
            let batch = remote
                .migration_inbox()
                .drain(limit, &mut fields.migration_buffer);
            (batch.drained(), batch.pending())
        };
        let mut detached = [InboxMessage::EMPTY; crate::DEFAULT_BATCH_LIMIT];
        detached[..drained].copy_from_slice(&cpu.migration_buffer[..drained]);
        let mut messages = DetachedOwnerMessageBatch::new(
            &detached[..drained],
            DetachedPayloadKind::SchedulerDelivery,
        );
        while let Some(message) = messages.next() {
            let operation = message.operation();
            if operation == InboxOperation::BalanceRequest {
                let source = message
                    .source_cpu()
                    .ok_or(TaskError::InvalidConfiguration)?;
                let target = message
                    .target_cpu()
                    .ok_or(TaskError::InvalidConfiguration)?;
                if source != cpu.owner() {
                    return Err(TaskError::CpuOwnerMismatch {
                        expected: source.as_u32(),
                        actual: cpu.owner().as_u32(),
                    });
                }
                let _source_epoch = message
                    .balance_source_epoch()
                    .ok_or(TaskError::InvalidConfiguration)?;
                let _migrated = self.transfer_owner_balance_candidate(
                    cpu.as_mut(),
                    target,
                    now_ns,
                    BalanceReason::IdlePull,
                )?;
                continue;
            }
            if matches!(
                operation,
                InboxOperation::RemoteWake
                    | InboxOperation::BalanceRequest
                    | InboxOperation::Reclaim
            ) {
                return Err(TaskError::InvalidConfiguration);
            }
            if message.payload() == 0 {
                continue;
            }
            // SAFETY: publication transfers one Arc count in the payload and
            // this detached owner message consumes that count exactly once.
            let core = unsafe {
                Arc::from_raw(ptr::with_exposed_provenance::<ThreadCore>(
                    message.payload(),
                ))
            };
            let _delivery = core.accept_scheduler_inbox_delivery();
            if core.id() != message.thread_id() {
                continue;
            }
            let Some(_activity) = core.try_scheduler_activity() else {
                // Exit owns the transition gate and will clear any pending
                // migration target before publishing the reaper retry.
                continue;
            };
            if core.state() == ThreadState::Exited {
                core.sched().lock().placement.set_migration_target(None)?;
                continue;
            }
            let owner = cpu.owner();
            let source = message
                .source_cpu()
                .ok_or(TaskError::InvalidConfiguration)?;
            let target = message
                .target_cpu()
                .ok_or(TaskError::InvalidConfiguration)?;
            if operation == InboxOperation::AffinityUpdate {
                if source != owner {
                    return Err(TaskError::CpuOwnerMismatch {
                        expected: source.as_u32(),
                        actual: owner.as_u32(),
                    });
                }
                self.reconcile_owner_affinity_update(cpu.as_mut(), &core)?;
                continue;
            }
            if operation == InboxOperation::PolicyUpdate {
                if source != owner || target != owner {
                    return Err(TaskError::CpuOwnerMismatch {
                        expected: source.as_u32(),
                        actual: owner.as_u32(),
                    });
                }
                let cleanup_deadline_member = {
                    let sched = core.sched().lock();
                    sched.deadline_cleanup_pending
                        && sched.deadline_bandwidth_cpu == Some(owner)
                        && sched.placement.queued_cpu().is_none()
                        && sched.placement.running_cpu().is_none()
                        && sched.placement.on_cpu().is_none()
                };
                if cleanup_deadline_member {
                    Self::detach_owner_deadline_bandwidth(&core, cpu.as_mut())?;
                    core.sched().lock().deadline_cleanup_pending = false;
                    continue;
                }
            }
            if operation == InboxOperation::Migration {
                if source == target {
                    return Err(TaskError::InvalidConfiguration);
                }
                if target != owner {
                    return Err(TaskError::CpuOwnerMismatch {
                        expected: target.as_u32(),
                        actual: owner.as_u32(),
                    });
                }
                let forward_target = {
                    let mut sched = core.sched().lock();
                    let Some(committed_target) = sched.placement.migration_target() else {
                        continue;
                    };
                    let latest_target =
                        if committed_target == owner && sched.affinity.contains(owner) {
                            owner
                        } else if committed_target != owner
                            && sched.affinity.contains(committed_target)
                            && self
                                .cpu_remotes
                                .get(committed_target.as_usize())
                                .is_some_and(|remote| remote.is_online())
                        {
                            committed_target
                        } else {
                            self.select_allowed_online_cpu(&sched.affinity, Some(owner))
                                .ok_or(TaskError::InvalidConfiguration)?
                        };
                    if latest_target != owner {
                        sched.placement.set_migration_target(Some(latest_target))?;
                        core.set_target_cpu(latest_target);
                        Some(latest_target)
                    } else {
                        if sched.lifecycle.state() != ThreadState::Ready
                            || sched.placement.queued_cpu().is_some()
                            || sched.placement.running_cpu().is_some()
                            || sched.placement.on_cpu().is_some()
                        {
                            return Err(TaskError::InvalidConfiguration);
                        }
                        sched.placement.set_migration_target(None)?;
                        core.set_target_cpu(owner);
                        None
                    }
                };
                if let Some(forward_target) = forward_target {
                    self.publish_owner_migration(&core, forward_target, owner, forward_target)?;
                } else {
                    self.enqueue_owner_thread(
                        cpu.as_mut(),
                        Arc::clone(&core),
                        now_ns,
                        EnqueueReason::Migrated,
                    )?;
                }
                continue;
            }
            debug_assert_eq!(operation, InboxOperation::PolicyUpdate);
            let (queued_cpu, running_cpu, policy_generation, cbs_borrowed) = {
                let sched = core.sched().lock();
                (
                    sched.placement.queued_cpu(),
                    sched.placement.running_cpu(),
                    sched.policy_generation,
                    sched.deadline_cbs_borrower.is_some(),
                )
            };
            if message.generation() > policy_generation {
                continue;
            }
            if cbs_borrowed {
                // The remote PI owner is the sole mutable owner of this CBS
                // entity until its next scheduler safe point. Re-publish the
                // cold-path policy update instead of replacing donor state
                // underneath an in-flight dispatch copy.
                self.publish_owner_policy_retry(&core, owner, policy_generation)?;
                cpu.request_scheduler_work();
                continue;
            }
            if queued_cpu == Some(owner) {
                if cpu.as_ref().get_ref().current_dispatch.is_some() {
                    cpu.as_mut().settle_current_dispatch(now_ns, 0)?;
                } else {
                    cpu.as_mut()
                        .fields_mut()
                        .run_queue
                        .update_fair_virtual_time(None);
                }
                let fair_placement =
                    Self::owner_fair_policy_placement(cpu.as_ref().get_ref(), &core);
                let queued = cpu
                    .as_mut()
                    .fields_mut()
                    .run_queue
                    .dequeue(core.id())
                    .ok_or(TaskError::NotReady)?;
                Self::detach_owner_deadline_bandwidth(&core, cpu.as_mut())?;
                {
                    let mut sched = core.sched().lock();
                    if !sched.is_pi_boosted() {
                        sched.base_entity = queued.entity;
                        sched.entity = queued.entity;
                    }
                    sched.placement.set_queued_cpu(None)?;
                }
                let applied = self.apply_owner_policy_generation(
                    &core,
                    message.generation(),
                    now_ns,
                    fair_placement,
                    true,
                )?;
                if applied {
                    self.recompute_pi_after_policy_update(core.id())?;
                }
                self.enqueue_owner_thread(
                    cpu.as_mut(),
                    Arc::clone(&core),
                    now_ns,
                    EnqueueReason::PolicyChanged,
                )?;
                cpu.request_reschedule();
            } else if running_cpu == Some(owner) && cpu.current() == Some(core.id()) {
                self.commit_owner_current_dispatch(cpu.as_mut(), now_ns)?;
                let fair_placement =
                    Self::owner_fair_policy_placement(cpu.as_ref().get_ref(), &core);
                Self::detach_owner_deadline_bandwidth(&core, cpu.as_mut())?;
                let applied = self.apply_owner_policy_generation(
                    &core,
                    message.generation(),
                    now_ns,
                    fair_placement,
                    true,
                )?;
                if applied {
                    self.recompute_pi_after_policy_update(core.id())?;
                }
                {
                    let mut sched = core.sched().lock();
                    Self::activate_owner_deadline_bandwidth(
                        &core,
                        &mut sched,
                        cpu.as_mut(),
                        owner,
                    )?;
                    let dispatch = Self::owner_dispatch(&core, &sched, now_ns)?;
                    cpu.as_mut().install_dispatch(dispatch);
                }
                self.publish_owner_cpu_load_summary(cpu.as_mut());
                cpu.request_reschedule();
            } else {
                if core.sched().lock().deadline_bandwidth_cpu == Some(owner) {
                    Self::detach_owner_deadline_bandwidth(&core, cpu.as_mut())?;
                }
                let applied = self.apply_owner_policy_generation(
                    &core,
                    message.generation(),
                    now_ns,
                    None,
                    false,
                )?;
                if applied {
                    self.recompute_pi_after_policy_update(core.id())?;
                }
                Self::assign_owner_inactive_deadline_bandwidth(&core, cpu.as_mut())?;
            }
        }
        if pending {
            cpu.request_scheduler_work();
        }
        Ok(RemoteWakeDrain { drained, pending })
    }

    /// Drains one bounded batch from every inbox owned by `cpu`.
    ///
    /// The inboxes, rather than `need_resched`, are the source of truth for
    /// remote scheduler work. Forced scheduling operations call this before
    /// claiming their doorbell so object-API users cannot accidentally clear a
    /// wake, migration, or policy update without first making it visible to the
    /// owner run queue. Work racing after this batch is retained by
    /// [`CpuLocal::scheduler_enter`]'s post-claim inbox recheck.
    fn drain_owner_work(&self, mut cpu: Pin<&mut CpuLocal>, now_ns: u64) -> Result<(), TaskError> {
        self.drain_remote_wakes(cpu.as_mut(), now_ns)?;
        self.drain_policy_updates(cpu.as_mut(), now_ns)?;
        if cpu.has_remote_work() {
            cpu.request_scheduler_work();
            // One safe point consumes at most one batch from each inbox. A
            // self-IPI carries the remainder into a later IRQ-return instead
            // of turning this safe point into an unbounded drain loop or
            // relying on a future periodic tick.
            let remote = Arc::clone(cpu.remote());
            remote.kick_scheduler_work();
        }
        Ok(())
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

const fn next_generation(generation: u32) -> u32 {
    generation.saturating_add(1)
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
