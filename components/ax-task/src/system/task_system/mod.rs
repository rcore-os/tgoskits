//! Generation-checked registry and scheduling orchestration.

mod cpu_lifecycle;
mod deferred_work;
mod lifecycle;
mod model;
mod outcome;
mod pi;
mod registry;
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

    /// Requests one owner-mediated pull from the busiest remote CPU.
    ///
    /// The target never locks or mutates the source runqueue. Its pinned request
    /// node is published to the source migration inbox and the source owner
    /// selects and hands off one affinity-compatible thread at a safe point.
    pub fn request_idle_pull(&self, cpu: Pin<&CpuLocal>) -> Result<bool, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        if task_runtime::in_hard_irq() {
            return Ok(false);
        }
        self.ensure_owner_cpu_online(&cpu)?;
        if cpu.try_runnable_summary() != Some(0) {
            return Ok(false);
        }
        let now_ns = task_runtime::monotonic_ns();
        let target = cpu.owner();
        let source = self
            .cpu_remotes
            .iter()
            .enumerate()
            .filter(|(index, remote)| remote.is_online() && CpuId::new(*index as u32) != target)
            .filter_map(|(index, local)| {
                let source = CpuId::new(index as u32);
                let summary = local.try_load_summary()?;
                let key = summary.pushable_key()?;
                if !summary.is_overloaded()
                    || (summary.pushable_class() == Some(SchedulingClass::Fair)
                        && !local.fair_balance_due(now_ns))
                {
                    return None;
                }
                Some((key, summary.runnable_count(), summary.epoch(), source))
            })
            .min_by_key(|(key, load, _, source)| {
                (*key, core::cmp::Reverse(*load), source.as_u32())
            });
        let Some((_, _, source_epoch, source)) = source else {
            return Ok(false);
        };
        let source_local = self
            .cpu_remote(source)
            .ok_or(TaskError::CpuOffline(source.as_u32()))?;
        let message = InboxMessage::balance_request(source, target, source_epoch);
        let result = source_local.publish_migration(cpu.balance_request_node(), message);
        Ok(matches!(
            result,
            PublishResult::Published | PublishResult::AlreadyPending
        ))
    }

    /// Pushes one queued thread from an overloaded owner to the least loaded CPU.
    ///
    /// Selection and dequeue happen only on `cpu`; the target receives an
    /// intrusive handoff and enqueues it in its own safe-point drain.
    pub fn push_overloaded(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
    ) -> Result<Option<ThreadId>, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        if task_runtime::in_hard_irq() {
            return Ok(None);
        }
        self.ensure_owner_cpu_online(&cpu)?;
        let source = cpu.owner();
        self.publish_owner_cpu_load_summary(cpu.as_mut());
        let Some(source_summary) = cpu.try_load_summary() else {
            return Ok(None);
        };
        if !source_summary.is_overloaded()
            || !matches!(
                source_summary.pushable_class(),
                Some(SchedulingClass::Deadline | SchedulingClass::Realtime)
            )
        {
            return Ok(None);
        }
        let target = self
            .cpu_remotes
            .iter()
            .enumerate()
            .filter(|(index, remote)| remote.is_online() && CpuId::new(*index as u32) != source)
            .filter_map(|(index, remote)| {
                let target = CpuId::new(index as u32);
                let target_summary = remote.try_load_summary()?;
                if target_summary.runnable_count() >= source_summary.runnable_count() {
                    return None;
                }
                let candidate = self.select_owner_balance_candidate(
                    cpu.as_ref().get_ref(),
                    Some(target),
                    0,
                    BalanceReason::RtDeadlinePush,
                )?;
                let key = candidate.entity.fair().map_or_else(
                    || {
                        candidate
                            .entity
                            .scheduling_key(candidate.policy, candidate.id.as_u64())
                    },
                    |fair| {
                        crate::SchedulingKey::new(
                            candidate.policy.class_rank(),
                            fair.virtual_deadline(),
                            candidate.id.as_u64(),
                        )
                    },
                );
                if target_summary
                    .current_key()
                    .is_some_and(|current| current <= key && current.class_rank() != 3)
                {
                    return None;
                }
                Some((key, target_summary.runnable_count(), target))
            })
            .min_by_key(|(key, load, target)| (*key, *load, target.as_u32()))
            .map(|(_, _, target)| target);
        let Some(target) = target else {
            return Ok(None);
        };
        self.transfer_owner_balance_candidate(
            cpu.as_mut(),
            target,
            task_runtime::monotonic_ns(),
            BalanceReason::RtDeadlinePush,
        )
    }

    /// Replenishes a throttled Deadline job and enqueues it on an owner CPU.
    pub fn replenish_deadline(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        thread: ThreadId,
        now_ns: u64,
    ) -> Result<(), TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        let core = {
            let state = self.state.lock();
            Arc::clone(&state.thread_record(thread)?.core)
        };
        {
            let mut sched = core.sched().lock();
            let mut deadline = sched.base_deadline.ok_or(TaskError::NotReady)?;
            deadline.replenish(now_ns);
            if deadline.is_throttled() {
                return Err(TaskError::NotReady);
            }
            match sched.lifecycle.state() {
                ThreadState::Blocked => {
                    sched.transition(&core, ThreadState::Waking)?;
                    sched.transition(&core, ThreadState::Ready)?;
                }
                ThreadState::Waking => sched.transition(&core, ThreadState::Ready)?,
                ThreadState::Ready => {}
                _ => return Err(TaskError::NotReady),
            }
            sched.base_deadline = Some(deadline);
            sched.base_entity = SchedulingEntity::Deadline(deadline);
            if !sched.is_pi_boosted() {
                sched.entity = sched.base_entity;
            }
            sched.deadline_replenish_pending = false;
        }
        self.enqueue_owner_thread(cpu.as_mut(), core, now_ns, EnqueueReason::Replenished)?;
        Self::program_local_timer(cpu.as_mut(), now_ns)
    }

    /// Charges the current dispatch and reports class budget expiration.
    pub fn charge_current(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
        runtime_ns: u64,
        reclaimed_ns: u64,
    ) -> Result<ChargeOutcome, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        if !cpu.is_online() {
            return Err(TaskError::CpuOffline(cpu.owner().as_u32()));
        }
        let charge = cpu
            .as_mut()
            .charge_current_dispatch(now_ns, runtime_ns, reclaimed_ns)?;
        Ok(ChargeOutcome {
            slice_expired: charge.slice_expired,
            deadline_overrun: charge.deadline_overrun,
        })
    }

    /// Charges exactly the unaccounted runtime since the current dispatch began
    /// or was last sampled.
    pub fn charge_current_until(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
        reclaimed_ns: u64,
    ) -> Result<ChargeOutcome, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        if !cpu.is_online() {
            return Err(TaskError::CpuOffline(cpu.owner().as_u32()));
        }
        let charge = cpu.as_mut().settle_current_dispatch(now_ns, reclaimed_ns)?;
        Ok(ChargeOutcome {
            slice_expired: charge.slice_expired,
            deadline_overrun: charge.deadline_overrun,
        })
    }

    /// Tests RT bandwidth, allowing a PI-boosted owner to run to unlock.
    pub fn rt_may_run(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
        pi_boosted_owner: bool,
    ) -> Result<bool, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        self.ensure_owner_cpu_online(&cpu)?;
        Ok(cpu
            .as_mut()
            .fields_mut()
            .rt_bandwidth
            .may_run(now_ns, pi_boosted_owner))
    }

    /// Selects the next thread according to strict class precedence.
    pub fn schedule(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<ScheduleDecision, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        self.complete_context_switch(cpu.as_mut())?;
        self.drain_owner_work(cpu.as_mut(), now_ns)?;
        self.ensure_owner_cpu_online(&cpu)?;
        cpu.as_mut().scheduler_enter();
        self.commit_owner_current_dispatch(cpu.as_mut(), now_ns)?;
        self.service_deadline_timers(cpu.as_mut(), now_ns)?;
        let previous = cpu.current();
        let previous_core = cpu.current_core().cloned();
        let mut migration_target = None;
        if let Some(core) = previous_core.as_ref() {
            migration_target = self.schedule_out_owner_running(
                cpu.as_mut(),
                Arc::clone(core),
                now_ns,
                EnqueueReason::Preempted,
            )?;
        }
        let next = self.pick_owner_next(cpu.as_mut(), now_ns, previous)?;
        if let Some(target) = next.outgoing_migration_target {
            migration_target = Some(target);
        }
        let next_core = next.core;
        Self::stage_switch_handoff(
            cpu.as_mut(),
            previous,
            previous_core.as_ref().map(Arc::clone),
            next_core.id(),
            migration_target,
        )?;
        let reason = if migration_target.is_some() {
            SwitchReason::Migrated
        } else {
            SwitchReason::Preempted
        };
        let decision = Self::owner_switch_plan(previous_core.as_ref(), &next_core, reason);
        Ok(self.finish_owner_selection(cpu, decision, now_ns))
    }

    /// Services sticky scheduler work and switches only for a real preemption.
    pub fn schedule_if_requested(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<SchedulerOutcome, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        self.complete_context_switch(cpu.as_mut())?;
        self.drain_owner_work(cpu.as_mut(), now_ns)?;
        self.ensure_owner_cpu_online(&cpu)?;
        if cpu.current_lifecycle_state() == Some(ThreadState::Parking) {
            // The interrupted owner still holds a generation-checked park
            // token and remains `current` / `on_cpu`. Consume this safe-point
            // doorbell so an IRQ-return `while need_resched` loop can return to
            // `commit_park`. A real preemption request is kept separately and
            // restored only if the park is cancelled.
            let preempt_requested = cpu.as_mut().scheduler_enter();
            cpu.defer_park_preemption(preempt_requested);
            return Ok(SchedulerOutcome::ParkingDeferred);
        }
        let mut switch_requested = cpu.as_mut().scheduler_enter();
        self.commit_owner_current_dispatch(cpu.as_mut(), now_ns)?;
        self.service_deadline_timers(cpu.as_mut(), now_ns)?;
        // Work published while this bounded safe point is running must affect
        // this decision. `scheduler_enter` consumes only the request observed
        // on entry; the second exchange closes the publication window without
        // losing a request that races after it.
        switch_requested |= cpu.take_preempt_requested();
        let previous = cpu.current();
        let previous_core = cpu.current_core().cloned();
        if let Some(core) = previous_core.as_ref()
            && !switch_requested
        {
            let dispatch = {
                let sched = core.sched().lock();
                Self::owner_dispatch(core, &sched, now_ns)?
            };
            cpu.as_mut().install_dispatch(dispatch);
            self.publish_owner_cpu_load_summary(cpu.as_mut());
            // `scheduler_enter` consumed the sticky entry request, but a
            // bounded inbox drain may have left another batch behind. Preserve
            // that work (and any request produced by Deadline servicing) for
            // the next scheduler safe point.
            if cpu.has_remote_work() {
                cpu.request_scheduler_work();
            }
            Self::program_local_timer(cpu.as_mut(), now_ns)?;
            return Ok(if cpu.has_remote_work() {
                SchedulerOutcome::OwnerWorkPending
            } else {
                SchedulerOutcome::Quiescent
            });
        }
        let mut migration_target = None;
        if let Some(core) = previous_core.as_ref() {
            migration_target = self.schedule_out_owner_running(
                cpu.as_mut(),
                Arc::clone(core),
                now_ns,
                EnqueueReason::Preempted,
            )?;
        }
        let next = self.pick_owner_next(cpu.as_mut(), now_ns, previous)?;
        if let Some(target) = next.outgoing_migration_target {
            migration_target = Some(target);
        }
        let next_core = next.core;
        Self::stage_switch_handoff(
            cpu.as_mut(),
            previous,
            previous_core.as_ref().map(Arc::clone),
            next_core.id(),
            migration_target,
        )?;
        let reason = if migration_target.is_some() {
            SwitchReason::Migrated
        } else {
            SwitchReason::Preempted
        };
        let decision = Self::owner_switch_plan(previous_core.as_ref(), &next_core, reason);
        Ok(SchedulerOutcome::Decision(
            self.finish_owner_selection(cpu, decision, now_ns),
        ))
    }

    /// Moves the current thread to its class tail and selects another thread.
    pub fn yield_current(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<ScheduleDecision, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        self.complete_context_switch(cpu.as_mut())?;
        self.drain_owner_work(cpu.as_mut(), now_ns)?;
        self.ensure_owner_cpu_online(&cpu)?;
        cpu.as_mut().scheduler_enter();
        self.commit_owner_current_dispatch(cpu.as_mut(), now_ns)?;
        self.service_deadline_timers(cpu.as_mut(), now_ns)?;
        let previous = cpu.current();
        let previous_core = cpu.current_core().cloned();
        let mut migration_target = None;
        if let Some(core) = previous_core.as_ref() {
            let deadline_job_ended = {
                let mut sched = core.sched().lock();
                if matches!(sched.active_base_policy, SchedulePolicy::Deadline(_))
                    && !sched.is_pi_boosted()
                {
                    if !sched.entity.yield_deadline_job() {
                        return Err(TaskError::InvalidConfiguration);
                    }
                    if let SchedulingEntity::Deadline(deadline) = sched.entity {
                        sched.base_entity = sched.entity;
                        sched.base_deadline = Some(deadline);
                        cpu.as_mut()
                            .arm_deferred_scheduler_deadline(deadline.next_scheduler_event_ns());
                    }
                    sched.placement.set_running_cpu(None)?;
                    sched.deadline_replenish_pending = true;
                    sched.transition(core, ThreadState::Blocked)?;
                    true
                } else {
                    false
                }
            };
            if deadline_job_ended {
                Self::mark_owner_deadline_non_contending(core, cpu.as_mut(), now_ns)?;
                cpu.as_mut().clear_current();
            } else {
                migration_target = self.schedule_out_owner_running(
                    cpu.as_mut(),
                    Arc::clone(core),
                    now_ns,
                    EnqueueReason::Yield,
                )?;
            }
        }
        let next = self.pick_owner_next(cpu.as_mut(), now_ns, previous)?;
        if let Some(target) = next.outgoing_migration_target {
            migration_target = Some(target);
        }
        let next_core = next.core;
        Self::stage_switch_handoff(
            cpu.as_mut(),
            previous,
            previous_core.as_ref().map(Arc::clone),
            next_core.id(),
            migration_target,
        )?;
        let decision =
            Self::owner_switch_plan(previous_core.as_ref(), &next_core, SwitchReason::Yield);
        Ok(self.finish_owner_selection(cpu, decision, now_ns))
    }

    /// Publishes `PARKING` after consuming a wake-before-park notification.
    pub fn prepare_park(&self, mut cpu: Pin<&mut CpuLocal>) -> Result<ParkPrepare, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        self.complete_context_switch(cpu.as_mut())?;
        self.ensure_owner_cpu_online(&cpu)?;
        let core = cpu.current_core().ok_or(TaskError::NoRunnableThread)?;
        if core.take_park_notification() {
            return Ok(ParkPrepare::Notified);
        }
        let generation = core.next_park_generation()?;
        core.sched().lock().transition(core, ThreadState::Parking)?;
        Ok(ParkPrepare::Prepared(ParkTicket::new(
            core.id(),
            generation,
        )))
    }

    /// Rechecks a prepared park and either cancels it or commits schedule-out.
    pub fn commit_park(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        token: &mut ParkTicket,
    ) -> Result<ParkCommit, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        if token.is_resolved() {
            return Err(TaskError::StaleThreadId);
        }
        let now_ns = task_runtime::monotonic_ns();
        self.drain_owner_work(cpu.as_mut(), now_ns)?;
        self.ensure_owner_cpu_online(&cpu)?;
        if cpu.current() != Some(token.thread()) {
            return Err(TaskError::StaleThreadId);
        }
        let previous_core = cpu
            .current_core()
            .cloned()
            .ok_or(TaskError::NoRunnableThread)?;
        let generation = previous_core.park_generation();
        if generation != token.generation() {
            return Err(TaskError::StaleThreadId);
        }
        let notified = previous_core.take_park_notification();
        if notified {
            previous_core
                .sched()
                .lock()
                .transition(&previous_core, ThreadState::Running)?;
            cpu.finish_park_preemption(true);
            token.mark_resolved();
            return Ok(ParkCommit::Notified);
        }
        cpu.as_mut().scheduler_enter();
        cpu.finish_park_preemption(false);
        self.commit_owner_current_dispatch(cpu.as_mut(), now_ns)?;
        {
            let mut sched = previous_core.sched().lock();
            sched.transition(&previous_core, ThreadState::Blocked)?;
            sched.placement.set_running_cpu(None)?;
        }
        Self::mark_owner_deadline_non_contending(&previous_core, cpu.as_mut(), now_ns)?;
        cpu.as_mut().clear_current();
        let next = self.pick_owner_next(cpu.as_mut(), now_ns, Some(token.thread()))?;
        if next.outgoing_migration_target.is_some() {
            return Err(TaskError::InvalidConfiguration);
        }
        let next_core = next.core;
        Self::stage_switch_handoff(
            cpu.as_mut(),
            Some(token.thread()),
            Some(Arc::clone(&previous_core)),
            next_core.id(),
            None,
        )?;
        let decision =
            Self::owner_switch_plan(Some(&previous_core), &next_core, SwitchReason::Blocked);
        let decision = self.finish_owner_selection(cpu, decision, now_ns);
        token.mark_resolved();
        Ok(ParkCommit::Blocked(decision))
    }

    /// Cancels a prepared park because an independent grant won the race.
    pub fn cancel_park(
        &self,
        cpu: Pin<&mut CpuLocal>,
        token: &mut ParkTicket,
    ) -> Result<(), TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        if token.is_resolved() {
            return Err(TaskError::StaleThreadId);
        }
        self.ensure_owner_cpu_online(&cpu)?;
        if cpu.current() != Some(token.thread()) {
            return Err(TaskError::StaleThreadId);
        }
        let core = cpu.current_core().ok_or(TaskError::NoRunnableThread)?;
        if core.park_generation() != token.generation() {
            return Err(TaskError::StaleThreadId);
        }
        core.sched().lock().transition(core, ThreadState::Running)?;
        cpu.finish_park_preemption(true);
        token.mark_resolved();
        Ok(())
    }

    /// Parks the current thread and selects its replacement.
    pub fn block_current(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
    ) -> Result<ScheduleDecision, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        match self.prepare_park(cpu.as_mut())? {
            ParkPrepare::Prepared(mut ticket) => {
                match self.commit_park(cpu.as_mut(), &mut ticket)? {
                    ParkCommit::Blocked(decision) => Ok(decision),
                    ParkCommit::Notified => {
                        let core = cpu.current_core().ok_or(TaskError::NoRunnableThread)?;
                        Ok(Self::owner_switch_plan(
                            Some(core),
                            core,
                            SwitchReason::Blocked,
                        ))
                    }
                }
            }
            ParkPrepare::Notified => {
                let core = cpu.current_core().ok_or(TaskError::NoRunnableThread)?;
                Ok(Self::owner_switch_plan(
                    Some(core),
                    core,
                    SwitchReason::Blocked,
                ))
            }
        }
    }

    /// Validates all fallible current-thread exit prerequisites without
    /// publishing the thread as exited.
    pub fn prepare_current_exit(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<ThreadId, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        self.complete_context_switch(cpu.as_mut())?;
        self.drain_owner_work(cpu.as_mut(), now_ns)?;
        let state = self.state.lock();
        state.ensure_cpu_online(&cpu)?;
        let current = cpu.current().ok_or(TaskError::NoRunnableThread)?;
        if cpu.idle() == Some(current) {
            return Err(TaskError::InvalidConfiguration);
        }
        let record = state.thread_record(current)?;
        let sched = record.sched.lock();
        let lifecycle = sched.lifecycle.state();
        if lifecycle != ThreadState::Running {
            return Err(TaskError::InvalidTransition {
                from: lifecycle,
                to: ThreadState::Exited,
            });
        }
        if record.blocked_on.is_some()
            || record.pi_waiter_head.is_some()
            || sched.blocked_pi_waiters != 0
        {
            return Err(TaskError::InvalidPiState);
        }
        if sched.placement.running_cpu() != Some(cpu.owner())
            || sched.placement.on_cpu() != Some(cpu.owner())
        {
            return Err(TaskError::ThreadBusy);
        }
        if record.resources.context().is_none() {
            return Err(TaskError::InvalidRuntimeHandle);
        }
        Ok(current)
    }

    /// Commits current-thread exit and selects a replacement.
    pub fn exit_current(&self, mut cpu: Pin<&mut CpuLocal>) -> Result<ScheduleDecision, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        self.complete_context_switch(cpu.as_mut())?;
        let now_ns = task_runtime::monotonic_ns();
        self.drain_owner_work(cpu.as_mut(), now_ns)?;
        self.commit_current_exit_after_owner_drain(cpu, now_ns)
    }

    /// Commits the non-returning half of current exit after owner work drained.
    ///
    /// The scheduler activity gate closes the intentional drain-to-commit
    /// window against a newly publishing remote policy or affinity update. A
    /// message that won before the gate remains an in-flight late delivery and
    /// pins registry resources until its owner drains it as an exited no-op.
    fn commit_current_exit_after_owner_drain(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<ScheduleDecision, TaskError> {
        let decision = {
            let mut state = self.state.lock();
            state.ensure_cpu_online(&cpu)?;
            let previous = cpu.current().ok_or(TaskError::NoRunnableThread)?;
            let previous_core = cpu.current_core().cloned();
            if state.thread_record(previous)?.has_live_pi_edges() {
                return Err(TaskError::InvalidPiState);
            }
            cpu.as_mut().scheduler_enter();
            self.commit_owner_current_dispatch(cpu.as_mut(), now_ns)?;
            let previous_core = previous_core.ok_or(TaskError::NoRunnableThread)?;
            Self::detach_owner_deadline_bandwidth(&previous_core, cpu.as_mut())?;
            let _exit = previous_core
                .try_scheduler_exit()
                .ok_or(TaskError::ThreadBusy)?;
            {
                let mut sched = previous_core.sched().lock();
                sched.placement.set_migration_target(None)?;
                sched.transition(&previous_core, ThreadState::Exited)?;
                sched.placement.mark_exited_awaiting_tail(cpu.owner())?;
                let record = state.thread_record_mut(previous)?;
                record.exit_callback_pending = record.extension.is_some();
                record.exit_callback_claimed = false;
            }
            state.release_deadline_reservation_on_exit(previous)?;
            cpu.as_mut().clear_current();
            let next = self.pick_owner_next(cpu.as_mut(), now_ns, Some(previous))?;
            if next.outgoing_migration_target.is_some() {
                return Err(TaskError::InvalidConfiguration);
            }
            let next_core = next.core;
            Self::stage_switch_handoff(
                cpu.as_mut(),
                Some(previous),
                Some(Arc::clone(&previous_core)),
                next_core.id(),
                None,
            )?;
            Self::owner_switch_plan(Some(&previous_core), &next_core, SwitchReason::Exited)
        };
        Ok(self.finish_owner_selection(cpu, decision, now_ns))
    }

    /// Completes the physical switch-out handoff in the newly active context.
    ///
    /// This second phase clears `on_cpu` only after architecture execution has
    /// left the previous stack. Deferred migration publication and exit hooks
    /// therefore cannot make a context runnable or reapable too early.
    pub fn complete_context_switch(&self, mut cpu: Pin<&mut CpuLocal>) -> Result<(), TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        let Some(initial_handoff) = cpu.as_ref().get_ref().switch_handoff().cloned() else {
            return Ok(());
        };
        let owner = cpu.owner();
        {
            let bandwidth = cpu.as_ref().get_ref().deadline_bandwidth();
            let sched = initial_handoff.previous.sched().lock();
            self.validate_switch_handoff_state(owner, bandwidth, &initial_handoff, &sched)?;
        }

        if !initial_handoff.runtime_tail_finished {
            ensure_runtime_success(task_runtime::finish_context_switch_tail())?;
            if cpu
                .as_mut()
                .finish_switch_runtime_tail(
                    initial_handoff.previous.id(),
                    initial_handoff.migration_target,
                )
                .is_err()
            {
                task_runtime::fatal_invariant(
                    0x5357_0001,
                    initial_handoff.previous.id().as_u64() as usize,
                );
            }
        }

        let handoff = cpu
            .as_ref()
            .get_ref()
            .switch_handoff()
            .cloned()
            .ok_or(TaskError::InvalidConfiguration)?;
        let previous = handoff.previous.id();
        let (migration_target, previous_exited) = {
            let bandwidth = cpu.as_ref().get_ref().deadline_bandwidth();
            let mut sched = handoff.previous.sched().lock();
            let (migration_target, previous_exited) =
                self.validate_switch_handoff_state(owner, bandwidth, &handoff, &sched)?;
            if migration_target.is_some() && sched.deadline_bandwidth_cpu.is_some() {
                cpu.as_mut().fields_mut().remove_deadline_bandwidth(
                    sched.deadline_bandwidth_scaled,
                    sched.deadline_activity != DeadlineActivity::Inactive,
                )?;
                sched.deadline_bandwidth_cpu = None;
                cpu.as_mut()
                    .fields_mut()
                    .unregister_deadline_member(&handoff.previous);
            }
            sched.placement.set_on_cpu(None)?;
            if let Some(target) = migration_target {
                handoff.previous.set_target_cpu(target);
            }
            (migration_target, previous_exited)
        };
        if let Some(target) = migration_target
            && self
                .publish_owner_migration(&handoff.previous, target, owner, target)
                .is_err()
        {
            // The target was validated online before scheduler state was
            // committed. CPU hot-unplug is unsupported, so losing it here
            // would strand a Ready thread after the physical switch.
            task_runtime::fatal_invariant(0x5357_0002, target.as_u32() as usize);
        }
        let consumed = cpu.as_mut().take_switch_handoff().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x5357_0003, previous.as_u64() as usize)
        });
        if consumed.previous.id() != previous
            || consumed.migration_target != handoff.migration_target
            || !consumed.runtime_tail_finished
        {
            task_runtime::fatal_invariant(0x5357_0004, previous.as_u64() as usize);
        }
        self.publish_owner_cpu_load_summary(cpu.as_mut());
        if previous_exited {
            self.task_work.publish();
        }
        Ok(())
    }

    fn validate_switch_handoff_state(
        &self,
        owner: CpuId,
        bandwidth: DeadlineBandwidthSnapshot,
        handoff: &super::cpu::SwitchHandoff,
        sched: &ThreadSchedState,
    ) -> Result<(Option<CpuId>, bool), TaskError> {
        if sched.placement.on_cpu() != Some(owner) {
            return Err(TaskError::InvalidConfiguration);
        }
        let migration_target = match handoff.migration_target {
            Some(_) => {
                let target = sched
                    .placement
                    .migration_target()
                    .ok_or(TaskError::InvalidConfiguration)?;
                if sched.lifecycle.state() != ThreadState::Ready
                    || sched.placement.queued_cpu().is_some()
                    || sched.placement.running_cpu().is_some()
                {
                    return Err(TaskError::InvalidConfiguration);
                }
                if self.cpu_remote(target).is_none() {
                    return Err(TaskError::CpuOffline(target.as_u32()));
                }
                if let Some(assigned) = sched.deadline_bandwidth_cpu {
                    if assigned != owner {
                        return Err(TaskError::CpuOwnerMismatch {
                            expected: assigned.as_u32(),
                            actual: owner.as_u32(),
                        });
                    }
                    if bandwidth.this_bw_scaled() < sched.deadline_bandwidth_scaled
                        || (sched.deadline_activity != DeadlineActivity::Inactive
                            && bandwidth.running_bw_scaled() < sched.deadline_bandwidth_scaled)
                    {
                        return Err(TaskError::InvalidConfiguration);
                    }
                }
                Some(target)
            }
            None => None,
        };
        Ok((
            migration_target,
            sched.lifecycle.state() == ThreadState::Exited,
        ))
    }

    /// Consumes a direct wake publication and changes a blocked thread to ready.
    pub fn consume_wake(&self, wake: &ThreadWakeHandle) -> Result<bool, TaskError> {
        let state = self.state.lock();
        Self::consume_wake_locked(&state, wake)
    }

    fn consume_wake_locked(
        state: &TaskSystemState,
        wake: &ThreadWakeHandle,
    ) -> Result<bool, TaskError> {
        let core = match state.thread_record(wake.thread_id()) {
            Ok(record) => Arc::clone(&record.core),
            // A late IRQ wake racing with reaping or slot reuse is an idempotent
            // no-op, not a registry lookup failure visible to the IRQ producer.
            Err(TaskError::StaleThreadId) => return Ok(false),
            Err(error) => return Err(error),
        };
        Self::consume_owner_wake(&core)
    }

    fn consume_owner_wake(core: &Arc<ThreadCore>) -> Result<bool, TaskError> {
        let mut sched = core.sched().lock();
        let lifecycle = sched.lifecycle.state();
        if !core.consume_wake(lifecycle == ThreadState::Parking) || lifecycle == ThreadState::Exited
        {
            return Ok(false);
        }
        if sched.deadline_replenish_pending {
            return Ok(false);
        }
        match lifecycle {
            ThreadState::Parking => Ok(false),
            ThreadState::Blocked => {
                sched.transition(core, ThreadState::Waking)?;
                let base_policy = sched.active_base_policy;
                sched.base_entity.reset_after_wake(base_policy);
                let effective_policy = sched.policy;
                sched.entity.reset_after_wake(effective_policy);
                sched.transition(core, ThreadState::Ready)?;
                Ok(true)
            }
            ThreadState::Ready | ThreadState::Running | ThreadState::Waking => Ok(false),
            ThreadState::New | ThreadState::Exited => Ok(false),
        }
    }

    fn enqueue_owner_thread(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        core: Arc<ThreadCore>,
        now_ns: u64,
        reason: EnqueueReason,
    ) -> Result<(), TaskError> {
        self.ensure_owner_cpu_online(&cpu)?;
        let mut sched = core.sched().lock();
        let preempts_current =
            self.enqueue_owner_thread_locked(cpu.as_mut(), &core, &mut sched, now_ns, reason)?;
        drop(sched);
        self.finish_owner_enqueue(cpu, reason, preempts_current);
        Ok(())
    }

    fn enqueue_owner_thread_locked(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        core: &Arc<ThreadCore>,
        sched: &mut ThreadSchedState,
        now_ns: u64,
        reason: EnqueueReason,
    ) -> Result<bool, TaskError> {
        let owner = cpu.owner();
        if sched.lifecycle.state() != ThreadState::Ready {
            return Err(TaskError::NotReady);
        }
        if !sched.affinity.contains(owner) {
            return Err(TaskError::InvalidCpu(owner.as_u32()));
        }
        let policy = sched.policy;
        let mut queued_entity = sched.entity;
        if matches!(reason, EnqueueReason::Wake) && matches!(policy, SchedulePolicy::Deadline(_)) {
            queued_entity.activate_deadline(now_ns);
            sched.entity = queued_entity;
            if !sched.is_pi_boosted()
                && let SchedulingEntity::Deadline(deadline) = queued_entity
            {
                sched.base_entity = queued_entity;
                sched.base_deadline = Some(deadline);
            }
        }
        Self::activate_owner_deadline_bandwidth(core, sched, cpu.as_mut(), owner)?;
        let fields = cpu.as_mut().fields_mut();
        let queued_entity = fields.run_queue.enqueue(
            core.id(),
            policy,
            queued_entity,
            Arc::clone(core),
            now_ns,
            reason,
        )?;
        let current_fair = fields
            .current_dispatch
            .as_ref()
            .and_then(|dispatch| dispatch.entity.fair());
        fields.run_queue.update_fair_virtual_time(current_fair);
        let fair_virtual_time = queued_entity.fair().map_or(0, |fair| {
            fields.run_queue.virtual_time_for_mode(fair.mode())
        });
        let preempts_current = fields.current_dispatch.as_ref().is_none_or(|current| {
            current.should_preempt(
                policy,
                queued_entity,
                fair_virtual_time,
                self.config.wakeup_granularity_ns(),
            )
        });
        sched.entity = queued_entity;
        if !sched.is_pi_boosted() {
            sched.base_entity = queued_entity;
        }
        core.publish_effective_schedule(policy, queued_entity);
        sched.placement.set_queued_cpu(Some(owner))?;
        core.set_target_cpu(owner);
        Ok(preempts_current)
    }

    fn finish_owner_enqueue(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        reason: EnqueueReason,
        preempts_current: bool,
    ) {
        let fields = cpu.as_mut().fields_mut();
        if matches!(
            reason,
            EnqueueReason::Wake | EnqueueReason::Replenished | EnqueueReason::Migrated
        ) && preempts_current
        {
            fields.request_reschedule();
        }
        self.publish_owner_cpu_load_summary(cpu.as_mut());
    }

    fn activate_owner_deadline_bandwidth(
        core: &Arc<ThreadCore>,
        sched: &mut ThreadSchedState,
        mut cpu: Pin<&mut CpuLocal>,
        owner: CpuId,
    ) -> Result<(), TaskError> {
        if !matches!(sched.active_base_policy, SchedulePolicy::Deadline(_)) {
            return Ok(());
        }
        let member_registered = cpu.as_mut().fields_mut().register_deadline_member(core)?;
        let bandwidth_result = match sched.deadline_bandwidth_cpu {
            None => cpu
                .as_mut()
                .fields_mut()
                .add_deadline_bandwidth(sched.deadline_bandwidth_scaled, true),
            Some(assigned) if assigned != owner => Err(TaskError::CpuOwnerMismatch {
                expected: assigned.as_u32(),
                actual: owner.as_u32(),
            }),
            Some(_) if sched.deadline_activity == DeadlineActivity::Inactive => cpu
                .as_mut()
                .fields_mut()
                .activate_deadline_bandwidth(sched.deadline_bandwidth_scaled),
            Some(_) => Ok(()),
        };
        if let Err(error) = bandwidth_result {
            if member_registered {
                cpu.as_mut().fields_mut().unregister_deadline_member(core);
            }
            return Err(error);
        }
        sched.deadline_activity = DeadlineActivity::ActiveContending;
        sched.deadline_bandwidth_cpu = Some(owner);
        sched.deadline_zero_lag_ns = 0;
        Ok(())
    }

    fn detach_owner_deadline_bandwidth(
        core: &Arc<ThreadCore>,
        cpu: Pin<&mut CpuLocal>,
    ) -> Result<(), TaskError> {
        let mut sched = core.sched().lock();
        Self::detach_owner_deadline_bandwidth_locked(core, &mut sched, cpu)
    }

    fn detach_owner_deadline_bandwidth_locked(
        core: &Arc<ThreadCore>,
        sched: &mut ThreadSchedState,
        mut cpu: Pin<&mut CpuLocal>,
    ) -> Result<(), TaskError> {
        let owner = cpu.owner();
        let Some(assigned_cpu) = sched.deadline_bandwidth_cpu else {
            return Ok(());
        };
        if assigned_cpu != owner {
            return Err(TaskError::CpuOwnerMismatch {
                expected: assigned_cpu.as_u32(),
                actual: owner.as_u32(),
            });
        }
        cpu.as_mut().fields_mut().remove_deadline_bandwidth(
            sched.deadline_bandwidth_scaled,
            sched.deadline_activity != DeadlineActivity::Inactive,
        )?;
        sched.deadline_bandwidth_cpu = None;
        cpu.as_mut().fields_mut().unregister_deadline_member(core);
        Ok(())
    }

    fn assign_owner_inactive_deadline_bandwidth(
        core: &Arc<ThreadCore>,
        mut cpu: Pin<&mut CpuLocal>,
    ) -> Result<(), TaskError> {
        let owner = cpu.owner();
        let mut sched = core.sched().lock();
        if !matches!(sched.active_base_policy, SchedulePolicy::Deadline(_)) {
            return Ok(());
        }
        let member_registered = cpu.as_mut().fields_mut().register_deadline_member(core)?;
        let bandwidth_result = match sched.deadline_bandwidth_cpu {
            None => cpu
                .as_mut()
                .fields_mut()
                .add_deadline_bandwidth(sched.deadline_bandwidth_scaled, false),
            Some(assigned) if assigned != owner => Err(TaskError::CpuOwnerMismatch {
                expected: assigned.as_u32(),
                actual: owner.as_u32(),
            }),
            Some(_) => Ok(()),
        };
        if let Err(error) = bandwidth_result {
            if member_registered {
                cpu.as_mut().fields_mut().unregister_deadline_member(core);
            }
            return Err(error);
        }
        if sched.deadline_bandwidth_cpu.is_some() {
            return Ok(());
        }
        sched.deadline_activity = DeadlineActivity::Inactive;
        sched.deadline_bandwidth_cpu = Some(owner);
        sched.deadline_zero_lag_ns = 0;
        Ok(())
    }

    fn mark_owner_deadline_non_contending(
        core: &Arc<ThreadCore>,
        mut cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<(), TaskError> {
        let owner = cpu.owner();
        let mut sched = core.sched().lock();
        let (Some(assigned_cpu), Some(deadline)) =
            (sched.deadline_bandwidth_cpu, sched.base_deadline)
        else {
            return Ok(());
        };
        if assigned_cpu != owner || sched.deadline_activity != DeadlineActivity::ActiveContending {
            return Ok(());
        }
        let zero_lag_ns = deadline_zero_lag_ns(deadline);
        if zero_lag_ns <= now_ns {
            cpu.as_mut()
                .fields_mut()
                .deactivate_deadline_bandwidth(sched.deadline_bandwidth_scaled)?;
            sched.deadline_activity = DeadlineActivity::Inactive;
            sched.deadline_zero_lag_ns = 0;
        } else {
            sched.deadline_activity = DeadlineActivity::ActiveNonContending;
            sched.deadline_zero_lag_ns = zero_lag_ns;
            cpu.arm_deferred_scheduler_deadline(zero_lag_ns);
        }
        Ok(())
    }

    fn owner_fair_policy_placement(
        cpu: &CpuLocal,
        core: &Arc<ThreadCore>,
    ) -> Option<FairPolicyPlacement> {
        let sched = core.sched().lock();
        let destination_mode = match sched.base_policy {
            SchedulePolicy::Fair { mode, .. } => mode,
            _ => return None,
        };
        let source_mode = sched
            .base_entity
            .fair()
            .map_or(destination_mode, |fair| fair.mode());
        Some(FairPolicyPlacement {
            source_virtual_time: cpu.run_queue.virtual_time_for_mode(source_mode),
            destination_virtual_time: cpu.run_queue.virtual_time_for_mode(destination_mode),
        })
    }

    fn owner_dispatch(
        core: &Arc<ThreadCore>,
        sched: &ThreadSchedState,
        now_ns: u64,
    ) -> Result<CurrentDispatch, TaskError> {
        let mut dispatch_policy = sched.policy;
        let mut dispatch_entity = sched.entity;
        let mut pi_critical_rescue = sched.pi_critical_rescue;
        let (donor_core, cbs_generation) =
            match (sched.deadline_donor, sched.deadline_donor_core.as_ref()) {
                (None, None) => (None, None),
                (Some(donor), Some(donor_core_weak)) => {
                    let donor_core = donor_core_weak.upgrade().ok_or(TaskError::InvalidPiState)?;
                    if donor_core.id() != donor {
                        return Err(TaskError::InvalidPiState);
                    }
                    let mut donor_sched = donor_core.sched().lock();
                    let policy = match donor_sched.active_base_policy {
                        SchedulePolicy::Deadline(policy) => SchedulePolicy::Deadline(policy),
                        _ => return Err(TaskError::InvalidPiState),
                    };
                    let deadline = donor_sched.base_deadline.ok_or(TaskError::InvalidPiState)?;
                    dispatch_policy = policy;
                    dispatch_entity = SchedulingEntity::Deadline(deadline);
                    // `on_cpu` remains set until architecture switch tail, after
                    // the outgoing dispatch has already been committed. The CBS
                    // is available as soon as the donor is neither the runnable
                    // owner dispatch nor a queued candidate; timer servicing is
                    // excluded by the borrower baton below.
                    let cbs_available = donor_sched.placement.running_cpu().is_none()
                        && donor_sched.placement.queued_cpu().is_none();
                    let cbs_generation =
                        if cbs_available && donor_sched.deadline_cbs_borrower.is_none() {
                            let generation = donor_sched
                                .deadline_cbs_generation
                                .checked_add(1)
                                .ok_or(TaskError::InvalidConfiguration)?;
                            donor_sched.deadline_cbs_generation = generation;
                            donor_sched.deadline_cbs_borrower = Some(core.id());
                            pi_critical_rescue = sched.blocked_pi_waiters != 0
                                && deadline.remaining_runtime_ns() == 0;
                            Some(generation)
                        } else {
                            // A running/queued donor still owns its local dispatch
                            // copy. Let the lock owner make bounded rescue progress,
                            // but do not debit or overwrite the donor CBS until the
                            // donor has completed its schedule-out handoff.
                            pi_critical_rescue = true;
                            None
                        };
                    drop(donor_sched);
                    (Some(donor_core), cbs_generation)
                }
                _ => return Err(TaskError::InvalidPiState),
            };
        Ok(CurrentDispatch::new(
            CurrentDispatchState {
                thread: core.id(),
                policy: dispatch_policy,
                entity: dispatch_entity,
                deadline_donor: sched.deadline_donor,
                blocks_pi_waiter: sched.blocked_pi_waiters != 0,
                rt_quota_exempt: sched.is_pi_boosted_rt_owner(),
                pi_critical_rescue,
                policy_generation: sched.dispatch_generation,
            },
            core,
            now_ns,
        )
        .with_deadline_donor_core(donor_core, cbs_generation))
    }

    fn commit_owner_current_dispatch(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<(), TaskError> {
        if cpu.as_ref().get_ref().current_dispatch.is_none() {
            return Ok(());
        }
        let _charge = cpu.as_mut().settle_current_dispatch(now_ns, 0)?;
        let Some(dispatch) = cpu.as_mut().take_dispatch() else {
            return Ok(());
        };
        if cpu.current() != Some(dispatch.thread)
            || cpu
                .current_core()
                .is_none_or(|core| !Arc::ptr_eq(core, dispatch.runtime_core_arc()))
        {
            return Err(TaskError::InvalidConfiguration);
        }
        dispatch.finish_runtime_accounting(now_ns);
        let mut deadline_task_work = false;
        let mut deadline_owner_reconcile = None;
        if let (Some(donor_core), Some(cbs_generation)) = (
            dispatch.deadline_donor_core(),
            dispatch.deadline_cbs_generation(),
        ) {
            let SchedulingEntity::Deadline(deadline) = dispatch.entity else {
                return Err(TaskError::InvalidPiState);
            };
            let mut donor = donor_core.sched().lock();
            if donor_core.id() != dispatch.deadline_donor.ok_or(TaskError::InvalidPiState)? {
                return Err(TaskError::InvalidPiState);
            }
            if donor.deadline_cbs_borrower != Some(dispatch.thread)
                || donor.deadline_cbs_generation != cbs_generation
            {
                return Err(TaskError::InvalidPiState);
            }
            let next_cbs_generation = donor
                .deadline_cbs_generation
                .checked_add(1)
                .ok_or(TaskError::InvalidConfiguration)?;
            let next_overrun_events = if dispatch.deadline_overrun {
                donor
                    .deadline_overrun_events
                    .checked_add(1)
                    .ok_or(TaskError::InvalidConfiguration)?
            } else {
                donor.deadline_overrun_events
            };
            donor.base_deadline = Some(deadline);
            donor.base_entity = SchedulingEntity::Deadline(deadline);
            if donor.deadline_activity == DeadlineActivity::ActiveNonContending {
                donor.deadline_zero_lag_ns = deadline_zero_lag_ns(deadline);
            }
            if matches!(donor.active_base_policy, SchedulePolicy::Deadline(_))
                && !donor.is_pi_boosted()
            {
                donor.entity = donor.base_entity;
            }
            donor.deadline_overrun_events = next_overrun_events;
            deadline_task_work |= dispatch.deadline_overrun;
            donor.deadline_cbs_borrower = None;
            donor.deadline_cbs_generation = next_cbs_generation;
            deadline_owner_reconcile = donor.deadline_bandwidth_cpu;
        }
        if let Some(owner) = deadline_owner_reconcile {
            let remote = self
                .cpu_remote(owner)
                .ok_or(TaskError::CpuOffline(owner.as_u32()))?;
            if owner == cpu.owner() {
                remote.request_scheduler_work();
            } else {
                // Baton return is a cross-CPU state publication. Publish the
                // donor owner's sticky work before the coalesced doorbell, so
                // a racing safe point either observes this request or a later
                // retry retains delivery ownership.
                remote.kick_scheduler_work();
            }
        }
        let mut sched = dispatch.runtime_core_arc().sched().lock();
        sched.charged_runtime_ns = sched
            .charged_runtime_ns
            .saturating_add(dispatch.charged_runtime_ns());
        if sched.dispatch_generation != dispatch.policy_generation {
            drop(sched);
            if deadline_task_work {
                dispatch.runtime_core_arc().publish_task_work();
            }
            return Ok(());
        }
        sched.entity = dispatch.entity;
        sched.pi_critical_rescue = dispatch.pi_critical_rescue;
        if !sched.is_pi_boosted() {
            sched.base_entity = dispatch.entity;
            if let SchedulingEntity::Deadline(deadline) = dispatch.entity {
                sched.base_deadline = Some(deadline);
            }
            if dispatch.deadline_overrun {
                sched.deadline_overrun_events = sched
                    .deadline_overrun_events
                    .checked_add(1)
                    .ok_or(TaskError::InvalidConfiguration)?;
                deadline_task_work = true;
            }
        }
        drop(sched);
        if deadline_task_work {
            dispatch.runtime_core_arc().publish_task_work();
        }
        Ok(())
    }

    fn apply_owner_policy_generation(
        &self,
        core: &Arc<ThreadCore>,
        generation: u64,
        now_ns: u64,
        fair_placement: Option<FairPolicyPlacement>,
        activate_deadline: bool,
    ) -> Result<bool, TaskError> {
        let mut sched = core.sched().lock();
        if generation > sched.policy_generation {
            return Ok(false);
        }
        if sched.applied_policy_generation == sched.policy_generation {
            return Ok(false);
        }
        let base_policy = sched.base_policy;
        let mut base_entity = match (sched.base_entity, base_policy) {
            (SchedulingEntity::Fair(fair), SchedulePolicy::Fair { nice, mode }) => {
                let source_virtual_time = fair_placement
                    .map(|placement| placement.source_virtual_time)
                    .unwrap_or_else(|| fair.vruntime());
                let destination_virtual_time = fair_placement
                    .map(|placement| placement.destination_virtual_time)
                    .unwrap_or(source_virtual_time);
                SchedulingEntity::Fair(fair.reconfigure(
                    nice,
                    mode,
                    source_virtual_time,
                    destination_virtual_time,
                ))
            }
            _ => SchedulingEntity::new(
                base_policy,
                self.config.fair_slice_ns(),
                fair_placement.map_or(0, |placement| placement.destination_virtual_time),
            ),
        };
        if activate_deadline {
            base_entity.activate_deadline(now_ns);
        }
        let previous_held = sched
            .active_deadline_reservation
            .max(sched.desired_deadline_reservation);
        sched.active_base_policy = base_policy;
        sched.base_entity = base_entity;
        sched.base_deadline = base_entity.deadline();
        if !sched.is_pi_boosted() {
            sched.policy = base_policy;
            sched.entity = base_entity;
        }
        sched.deadline_bandwidth_scaled = sched.desired_deadline_reservation;
        if sched.deadline_bandwidth_cpu.is_none() {
            sched.deadline_activity = DeadlineActivity::Inactive;
            sched.deadline_zero_lag_ns = 0;
        }
        sched.active_deadline_reservation = sched.desired_deadline_reservation;
        sched.applied_policy_generation = sched.policy_generation;
        sched.dispatch_generation = sched
            .dispatch_generation
            .checked_add(1)
            .ok_or(TaskError::InvalidConfiguration)?;
        let released = previous_held.saturating_sub(sched.desired_deadline_reservation);
        let effective_policy = sched.policy;
        let effective_entity = sched.entity;
        core.publish_effective_schedule(effective_policy, effective_entity);
        drop(sched);
        self.defer_deadline_admission_release(released)?;
        Ok(true)
    }

    fn recompute_pi_after_policy_update(&self, thread: ThreadId) -> Result<(), TaskError> {
        let mut state = self.state.lock();
        let recompute = state.prepare_pi_recompute_chain(thread)?;
        state.apply_pi_recompute_chain(recompute, self.config.fair_slice_ns());
        Ok(())
    }

    fn publish_owner_migration(
        &self,
        core: &Arc<ThreadCore>,
        inbox_cpu: CpuId,
        source: CpuId,
        target: CpuId,
    ) -> Result<(), TaskError> {
        let remote = self
            .cpu_remote(inbox_cpu)
            .ok_or(TaskError::CpuOffline(inbox_cpu.as_u32()))?;
        if !core.reserve_scheduler_inbox_delivery() {
            return Ok(());
        }
        let pointer = Arc::as_ptr(core);
        unsafe {
            // The retained count is transferred to the intrusive inbox.
            Arc::increment_strong_count(pointer);
        }
        let node = unsafe {
            // The transferred Arc count keeps the embedded node pinned.
            Pin::new_unchecked((*pointer).migration_node())
        };
        let message = InboxMessage::migration_with_payload(
            core.id(),
            source,
            target,
            core.id().generation() as u64,
            pointer.expose_provenance(),
        );
        if remote.publish_migration(node, message) != PublishResult::Published {
            unsafe {
                // A rejected/coalesced publication did not consume this count.
                Arc::decrement_strong_count(pointer);
            }
            core.cancel_scheduler_inbox_delivery();
        }
        Ok(())
    }

    fn publish_owner_policy_retry(
        &self,
        core: &Arc<ThreadCore>,
        owner: CpuId,
        generation: u64,
    ) -> Result<(), TaskError> {
        let remote = self
            .cpu_remote(owner)
            .ok_or(TaskError::CpuOffline(owner.as_u32()))?;
        if !core.reserve_scheduler_inbox_delivery() {
            return Ok(());
        }
        let pointer = Arc::as_ptr(core);
        // SAFETY: this count is transferred to the embedded inbox node and
        // consumed by exactly one later owner drain.
        unsafe { Arc::increment_strong_count(pointer) };
        // SAFETY: the transferred Arc count keeps the embedded node pinned.
        let node = unsafe { Pin::new_unchecked((*pointer).policy_update_node()) };
        let message = InboxMessage::policy_update_with_payload(
            core.id(),
            owner,
            generation,
            pointer.expose_provenance(),
        );
        if remote.publish_policy_update(node, message) != PublishResult::Published {
            // SAFETY: rejected/coalesced publication did not consume this
            // attempt's retained reference.
            unsafe { Arc::decrement_strong_count(pointer) };
            core.cancel_scheduler_inbox_delivery();
        }
        Ok(())
    }

    fn publish_owner_affinity_retry(
        &self,
        core: &Arc<ThreadCore>,
        owner: CpuId,
        target: CpuId,
    ) -> Result<(), TaskError> {
        let remote = self
            .cpu_remote(owner)
            .ok_or(TaskError::CpuOffline(owner.as_u32()))?;
        if !core.reserve_scheduler_inbox_delivery() {
            return Ok(());
        }
        let pointer = Arc::as_ptr(core);
        // SAFETY: this count is transferred to the dedicated affinity node and
        // consumed by exactly one later owner drain.
        unsafe { Arc::increment_strong_count(pointer) };
        // SAFETY: the transferred Arc count pins the embedded control node.
        let node = unsafe { Pin::new_unchecked((*pointer).affinity_update_node()) };
        let message = InboxMessage::affinity_update_with_payload(
            core.id(),
            owner,
            target,
            pointer.expose_provenance(),
        );
        if remote.publish_policy_update(node, message) != PublishResult::Published {
            // SAFETY: rejected/coalesced publication did not consume this
            // attempt's retained reference.
            unsafe { Arc::decrement_strong_count(pointer) };
            core.cancel_scheduler_inbox_delivery();
        }
        Ok(())
    }

    /// Changes thread affinity after validating Deadline root-domain coverage.
    pub fn set_affinity(&self, thread: ThreadId, affinity: CpuSet) -> Result<(), TaskError> {
        validate_affinity(&affinity, self.config.cpu_count())?;
        let state = self.state.lock();
        let root_domain = self.root_domain.lock();
        let record = state.thread_record(thread)?;
        let core = Arc::clone(&record.core);
        let mut sched = record.sched.lock();
        if sched.lifecycle.state() == ThreadState::Exited {
            return Err(TaskError::NotReady);
        }
        let is_deadline = matches!(sched.active_base_policy, SchedulePolicy::Deadline(_))
            || matches!(sched.base_policy, SchedulePolicy::Deadline(_));
        if is_deadline && !affinity.covers(&root_domain.online) {
            return Err(TaskError::DeadlineAffinity);
        }
        let timer_cpu = core.sleep_timer_cpu();
        if timer_cpu.is_some_and(|cpu| !affinity.contains(cpu)) {
            return Err(TaskError::ActiveTimerAffinity);
        }
        let target = timer_cpu
            .or_else(|| state.select_allowed_cpu(&affinity))
            .ok_or(TaskError::InvalidConfiguration)?;
        sched.affinity = affinity;
        // The affinity mask is task metadata, but physical placement belongs
        // to one runqueue owner. A remote writer only publishes a reconciliation
        // request; it never rewrites Queued/Running/SwitchingOut in place.
        let owner = sched
            .placement
            .running_cpu()
            .or(sched.placement.queued_cpu())
            .or(sched.placement.on_cpu())
            .or(sched.placement.migration_target())
            .or(sched.deadline_bandwidth_cpu);
        let target = owner
            .filter(|owner| sched.affinity.contains(*owner))
            .unwrap_or(target);
        core.set_target_cpu(target);
        drop(sched);
        if let Some(owner) = owner {
            state.publish_affinity_update(&core, owner, target)?;
        }
        Ok(())
    }

    /// Updates the owner CPU's running thread without publishing a self inbox.
    ///
    /// The caller owns `cpu` in an IRQ-off scheduler-safe window. A `true`
    /// result means the current thread must schedule out before the operation
    /// can return to its caller; switch tail will publish the detached context
    /// to the selected destination CPU.
    pub fn set_current_affinity(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        affinity: CpuSet,
    ) -> Result<bool, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        validate_affinity(&affinity, self.config.cpu_count())?;
        let state = self.state.lock();
        let root_domain = self.root_domain.lock();
        state.ensure_cpu_online(&cpu)?;
        let current = cpu.current().ok_or(TaskError::NoRunnableThread)?;
        let record = state.thread_record(current)?;
        let mut sched = record.sched.lock();
        if sched.placement.running_cpu() != Some(cpu.owner())
            || sched.placement.on_cpu() != Some(cpu.owner())
        {
            return Err(TaskError::InvalidConfiguration);
        }
        let is_deadline = matches!(sched.active_base_policy, SchedulePolicy::Deadline(_))
            || matches!(sched.base_policy, SchedulePolicy::Deadline(_));
        if is_deadline && !affinity.covers(&root_domain.online) {
            return Err(TaskError::DeadlineAffinity);
        }
        let timer_cpu = record.core.sleep_timer_cpu();
        if timer_cpu.is_some_and(|timer_cpu| !affinity.contains(timer_cpu)) {
            return Err(TaskError::ActiveTimerAffinity);
        }
        let target = timer_cpu
            .or_else(|| state.select_allowed_cpu(&affinity))
            .ok_or(TaskError::InvalidConfiguration)?;
        let owner = cpu.owner();
        let must_migrate = !affinity.contains(owner);
        sched.affinity = affinity;
        sched
            .placement
            .set_migration_target(must_migrate.then_some(target))?;
        record
            .core
            .set_target_cpu(if must_migrate { target } else { owner });
        drop(sched);
        if must_migrate {
            cpu.request_reschedule();
        }
        self.publish_owner_cpu_load_summary(cpu.as_mut());
        Ok(must_migrate)
    }

    /// Installs an idle thread for a CPU; idle is selected only when queues empty.
    pub fn install_idle_thread(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        thread: ThreadId,
    ) -> Result<(), TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        let state = self.state.lock();
        state.cpu_registration(cpu.owner())?;
        let core = Arc::clone(&state.thread_record(thread)?.core);
        cpu.as_mut().set_idle(thread, core);
        Ok(())
    }

    /// Returns Deadline budget and PI rescue state for diagnostics and ABI glue.
    pub fn deadline_runtime(&self, thread: ThreadId) -> Result<DeadlineRuntimeSnapshot, TaskError> {
        let state = self.state.lock();
        let record = state.thread_record(thread)?;
        let sched = record.sched.lock();
        let deadline = sched
            .base_deadline
            .or(match sched.entity {
                SchedulingEntity::Deadline(deadline) => Some(deadline),
                _ => None,
            })
            .ok_or(TaskError::InvalidConfiguration)?;
        Ok(DeadlineRuntimeSnapshot {
            remaining_runtime_ns: deadline.remaining_runtime_ns(),
            misses: deadline.misses(),
            overruns: deadline.overruns(),
            pi_critical_rescue: sched.pi_critical_rescue,
            donor: sched.deadline_donor,
        })
    }

    /// Returns the thread's GRUB activity, zero-lag, and runqueue ownership.
    pub fn deadline_activity(
        &self,
        thread: ThreadId,
    ) -> Result<DeadlineActivitySnapshot, TaskError> {
        let state = self.state.lock();
        let record = state.thread_record(thread)?;
        let sched = record.sched.lock();
        if !matches!(sched.active_base_policy, SchedulePolicy::Deadline(_)) {
            return Err(TaskError::InvalidConfiguration);
        }
        Ok(DeadlineActivitySnapshot {
            activity: sched.deadline_activity,
            bandwidth_cpu: sched.deadline_bandwidth_cpu,
            zero_lag_ns: sched.deadline_zero_lag_ns,
        })
    }

    /// Runs a bounded, allocation-free batch of deferred Deadline callbacks.
    ///
    /// Timer IRQ only publishes pending state. This task-context operation drops
    /// the registry lock before invoking any OS extension callback. Callback
    /// collection retains one existing thread-core reference at a time instead
    /// of allocating temporary storage in a scheduler-adjacent safe point.
    ///
    /// # Errors
    ///
    /// Returns [`TaskError::UnsafeContext`] without consuming an event in hard
    /// IRQ context, and [`TaskError::ThreadBusy`] when another task-work
    /// consumer is already active.
    pub fn dispatch_deadline_overruns(&self, limit: usize) -> Result<usize, TaskError> {
        if task_runtime::in_hard_irq() {
            return Err(TaskError::UnsafeContext);
        }
        let _consumer = self.task_work.try_claim_consumer()?;
        self.dispatch_deadline_overruns_inner(limit)
            .map(|(_, dispatched)| dispatched)
    }

    fn dispatch_deadline_overruns_inner(&self, limit: usize) -> Result<(usize, usize), TaskError> {
        const MAX_DISPATCH_BATCH: usize = 64;

        let mut processed = 0;
        let mut dispatched = 0;
        while processed < limit.min(MAX_DISPATCH_BATCH) {
            let claimed = {
                let mut state = self.state.lock();
                state.claim_pending_deadline_overrun()
            };
            let Some(callback) = claimed else {
                break;
            };
            processed += 1;
            let Some((extension, thread)) = callback else {
                continue;
            };
            // SAFETY: the registry's callback claim prevents reaping while the
            // callback runs, and every scheduler lock was released above.
            unsafe {
                (extension.ops().on_deadline_overrun)(extension.data(), thread);
            }
            self.state.lock().finish_deadline_callback(thread)?;
            dispatched += 1;
        }
        Ok((processed, dispatched))
    }

    /// Captures stable state for deterministic scheduler comparisons.
    pub fn snapshot(&self, cpu: Pin<&CpuLocal>) -> Result<CpuSnapshot, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        Ok(CpuSnapshot::capture(&cpu))
    }

    /// Returns the number of CPUs currently available for placement.
    pub fn online_cpu_count(&self) -> usize {
        loop {
            let sequence = self.topology_sequence.read_begin();
            let count = self.online_count.load(Ordering::Acquire);
            if !self.topology_sequence.read_retry(sequence) {
                return count;
            }
        }
    }

    fn publish_owner_cpu_load_summary(&self, mut cpu: Pin<&mut CpuLocal>) {
        // Linux protects runqueue load state with the owner rq lock and local
        // preemption exclusion. Keep the complete owner snapshot transaction
        // non-preemptible so the sequence cannot remain odd while an interrupt
        // recursively enters scheduler code on this CPU.
        let _irq = IrqScope::enter();
        let fields = cpu.as_mut().fields_mut();
        let current_key = fields
            .current_dispatch
            .as_ref()
            .map(CurrentDispatch::scheduling_key);
        let current_non_idle = fields.current.is_some() && fields.current != fields.idle;
        let candidate =
            self.select_owner_balance_candidate(fields, None, u64::MAX, BalanceReason::Summary);
        let pushable_key = candidate.map(|candidate| {
            candidate.entity.fair().map_or_else(
                || {
                    candidate
                        .entity
                        .scheduling_key(candidate.policy, candidate.id.as_u64())
                },
                |fair| {
                    crate::SchedulingKey::new(
                        candidate.policy.class_rank(),
                        fair.virtual_deadline(),
                        candidate.id.as_u64(),
                    )
                },
            )
        });
        let workload = fields
            .run_queue
            .len()
            .saturating_add(usize::from(current_non_idle));
        fields.publish_load_summary(
            current_key,
            pushable_key,
            fields.run_queue.len(),
            pushable_key.is_some() && workload > 1,
        );
    }

    fn select_owner_balance_candidate(
        &self,
        cpu: &CpuLocal,
        target: Option<CpuId>,
        now_ns: u64,
        reason: BalanceReason,
    ) -> Option<QueuedThread> {
        let source = cpu.owner();
        let current_policy = cpu
            .current_dispatch
            .as_ref()
            .map(CurrentDispatch::schedule_policy);
        let queued_top_rt = cpu.run_queue.highest_rt_priority();
        let top_rt_count =
            queued_top_rt.map_or(0, |priority| cpu.run_queue.rt_count_at_priority(priority));
        cpu.run_queue.balance_candidate(|candidate| {
            let sched = candidate.core.sched().lock();
            let target_is_allowed = |target: CpuId| {
                self.cpu_remotes
                    .get(target.as_usize())
                    .is_some_and(|remote| {
                        remote.is_online()
                            && remote.is_scheduler_ready()
                            && sched.affinity.contains(target)
                    })
            };
            let allowed_target = target.map_or_else(
                || {
                    self.cpu_remotes.iter().enumerate().any(|(index, _)| {
                        let target = CpuId::new(index as u32);
                        target != source && target_is_allowed(target)
                    })
                },
                target_is_allowed,
            );
            let deadline_covers_online =
                !matches!(sched.active_base_policy, SchedulePolicy::Deadline(_))
                    || self.cpu_remotes.iter().enumerate().all(|(index, remote)| {
                        !remote.is_online() || sched.affinity.contains(CpuId::new(index as u32))
                    });
            if !allowed_target
                || sched.placement.queued_cpu() != Some(source)
                || sched.placement.migration_target().is_some()
                || sched.placement.on_cpu().is_some()
                || candidate.core.sleep_timer_cpu().is_some()
                || !deadline_covers_online
            {
                return false;
            }
            let class_allowed = match reason {
                BalanceReason::Summary | BalanceReason::IdlePull => {
                    !matches!(
                        candidate.policy,
                        SchedulePolicy::Fair {
                            mode: FairMode::Idle,
                            ..
                        }
                    ) && (!matches!(candidate.policy, SchedulePolicy::Fair { .. })
                        || cpu.fair_balance_due(now_ns))
                }
                BalanceReason::RtDeadlinePush => matches!(
                    candidate.policy,
                    SchedulePolicy::Deadline(_)
                        | SchedulePolicy::Fifo { .. }
                        | SchedulePolicy::RoundRobin { .. }
                ),
                BalanceReason::FairPeriodic => matches!(
                    candidate.policy,
                    SchedulePolicy::Fair {
                        mode: FairMode::Normal | FairMode::Batch,
                        ..
                    }
                ),
            };
            if !class_allowed {
                return false;
            }
            let candidate_priority = match candidate.policy {
                SchedulePolicy::Fifo { priority } | SchedulePolicy::RoundRobin { priority, .. } => {
                    priority.get()
                }
                _ => return true,
            };
            match current_policy {
                Some(SchedulePolicy::Deadline(_)) => true,
                Some(SchedulePolicy::Fifo { priority })
                | Some(SchedulePolicy::RoundRobin { priority, .. }) => {
                    candidate_priority <= priority.get()
                }
                _ => queued_top_rt.is_some_and(|top| {
                    candidate_priority < top || (candidate_priority == top && top_rt_count > 1)
                }),
            }
        })
    }

    fn transfer_owner_balance_candidate(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        target: CpuId,
        now_ns: u64,
        reason: BalanceReason,
    ) -> Result<Option<ThreadId>, TaskError> {
        self.ensure_owner_cpu_online(&cpu)?;
        self.cpu_remote(target)
            .ok_or(TaskError::CpuOffline(target.as_u32()))?;
        let source = cpu.owner();
        if source == target {
            return Ok(None);
        }
        let Some(candidate) = self.select_owner_balance_candidate(
            cpu.as_ref().get_ref(),
            Some(target),
            now_ns,
            reason,
        ) else {
            return Ok(None);
        };
        let core = Arc::clone(&candidate.core);
        let queued = cpu
            .as_mut()
            .fields_mut()
            .run_queue
            .dequeue(core.id())
            .ok_or(TaskError::NotReady)?;
        Self::detach_owner_deadline_bandwidth(&core, cpu.as_mut())?;
        {
            let mut sched = core.sched().lock();
            if sched.lifecycle.state() != ThreadState::Ready
                || sched.placement.queued_cpu() != Some(source)
            {
                return Err(TaskError::InvalidConfiguration);
            }
            sched.entity = queued.entity;
            if !sched.is_pi_boosted() {
                sched.base_entity = queued.entity;
            }
            sched.placement.set_queued_cpu(None)?;
            sched.placement.set_migration_target(Some(target))?;
            core.set_target_cpu(target);
        }
        let migrated_fair = matches!(candidate.policy, SchedulePolicy::Fair { .. });
        self.publish_owner_cpu_load_summary(cpu.as_mut());
        self.publish_owner_migration(&core, target, source, target)?;
        if migrated_fair && reason != BalanceReason::FairPeriodic {
            let completion_now_ns = Self::scheduler_completion_now_ns(now_ns);
            cpu.as_mut()
                .reset_fair_balance(completion_now_ns, self.config.balance_interval_ns());
        }
        Ok(Some(core.id()))
    }

    fn service_deadline_timers(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<(), TaskError> {
        let member_count = cpu.deadline_members.len();
        if member_count == 0 {
            cpu.as_mut().refresh_scheduler_deadline(now_ns);
            return Ok(());
        }
        let owner = cpu.owner();
        let start = cpu.deadline_scan_cursor() % member_count;
        let examined = member_count.min(cpu.batch_limit());
        for offset in 0..examined {
            let index = (start + offset) % member_count;
            let core = Arc::clone(&cpu.deadline_members[index]);
            let mut update_queued = None;
            let mut replenish = false;
            {
                let mut sched = core.sched().lock();
                if sched.deadline_bandwidth_cpu != Some(owner) {
                    return Err(TaskError::CpuOwnerMismatch {
                        expected: sched.deadline_bandwidth_cpu.map_or(u32::MAX, CpuId::as_u32),
                        actual: owner.as_u32(),
                    });
                }
                if sched.deadline_cbs_borrower.is_some() {
                    // The remote PI owner holds the only mutable copy of this
                    // CBS entity in its CurrentDispatch. Its CPU owns the
                    // corresponding execution-budget clockevent until the
                    // baton is committed back below a scheduler safe point.
                    // Re-arming the donor's stale copy would give two CPUs
                    // timer ownership and, once overdue, create a resolution-
                    // rate interrupt loop without advancing CBS state.
                    continue;
                }
                if sched.deadline_activity == DeadlineActivity::ActiveNonContending {
                    if now_ns >= sched.deadline_zero_lag_ns {
                        cpu.as_mut()
                            .fields_mut()
                            .deactivate_deadline_bandwidth(sched.deadline_bandwidth_scaled)?;
                        sched.deadline_activity = DeadlineActivity::Inactive;
                        sched.deadline_zero_lag_ns = 0;
                    } else {
                        cpu.arm_deferred_scheduler_deadline(sched.deadline_zero_lag_ns);
                    }
                }
                let Some(mut deadline) = sched.base_deadline else {
                    continue;
                };
                let missed = deadline.observe_time(now_ns);
                let replenish_due =
                    deadline.is_throttled() && now_ns >= deadline.next_scheduler_event_ns();
                let next_event_ns = deadline.next_scheduler_event_ns();
                if !replenish_due && next_event_ns > now_ns {
                    cpu.arm_deferred_scheduler_deadline(next_event_ns);
                }
                if replenish_due {
                    deadline.replenish(now_ns);
                    sched.base_deadline = Some(deadline);
                    sched.base_entity = SchedulingEntity::Deadline(deadline);
                    if !sched.is_pi_boosted() {
                        sched.entity = sched.base_entity;
                        core.publish_effective_schedule(sched.policy, sched.entity);
                    }
                    if deadline.is_throttled() {
                        cpu.arm_deferred_scheduler_deadline(deadline.next_scheduler_event_ns());
                        continue;
                    }
                    if sched.deadline_replenish_pending {
                        sched.deadline_replenish_pending = false;
                        match sched.lifecycle.state() {
                            ThreadState::Blocked => {
                                sched.transition(&core, ThreadState::Waking)?;
                                sched.transition(&core, ThreadState::Ready)?;
                            }
                            ThreadState::Waking => sched.transition(&core, ThreadState::Ready)?,
                            ThreadState::Ready => {}
                            _ => return Err(TaskError::InvalidConfiguration),
                        }
                        replenish = true;
                    } else if !sched.is_pi_boosted() && sched.placement.queued_cpu() == Some(owner)
                    {
                        update_queued = Some(SchedulingEntity::Deadline(deadline));
                    }
                } else if missed {
                    sched.base_deadline = Some(deadline);
                    sched.base_entity = SchedulingEntity::Deadline(deadline);
                    if !sched.is_pi_boosted() {
                        sched.entity = sched.base_entity;
                        if sched.placement.queued_cpu() == Some(owner) {
                            update_queued = Some(SchedulingEntity::Deadline(deadline));
                        }
                    }
                }
            }
            if let Some(entity) = update_queued
                && !cpu
                    .as_mut()
                    .fields_mut()
                    .run_queue
                    .update_deadline_entity(core.id(), entity)
            {
                return Err(TaskError::InvalidConfiguration);
            }
            if replenish {
                self.enqueue_owner_thread(cpu.as_mut(), core, now_ns, EnqueueReason::Replenished)?;
            }
        }
        cpu.as_mut()
            .fields_mut()
            .set_deadline_scan_cursor((start + examined) % member_count);
        if examined < member_count {
            cpu.request_scheduler_work();
        }
        cpu.as_mut().refresh_scheduler_deadline(now_ns);
        Ok(())
    }

    /// Returns a monotonic sample suitable for work scheduled after this
    /// scheduler operation completes.
    ///
    /// Runtime accounting deliberately uses one entry snapshot throughout a
    /// scheduling decision. Deadline publication has different semantics: its
    /// relative intervals start when the scheduler returns work to task
    /// context. Like Linux hrtimer interrupt reprogramming, resample after
    /// potentially expensive callbacks or balancing and never move backwards
    /// from the caller's coherent accounting snapshot.
    fn scheduler_completion_now_ns(entry_now_ns: u64) -> u64 {
        task_runtime::monotonic_ns().max(entry_now_ns)
    }

    fn program_local_timer(
        mut cpu: Pin<&mut CpuLocal>,
        entry_now_ns: u64,
    ) -> Result<(), TaskError> {
        let completion_now_ns = Self::scheduler_completion_now_ns(entry_now_ns);
        cpu.as_mut().refresh_scheduler_deadline(completion_now_ns);
        let resolution_ns = task_runtime::timer_resolution_ns();
        let update = cpu
            .as_mut()
            .next_task_deadline_update(completion_now_ns, resolution_ns)?;
        ensure_runtime_success(task_runtime::publish_task_deadline(update))
    }

    /// Completes every owner-side selection through the same balance and
    /// one-shot programming sequence.
    ///
    /// Forced block and exit paths select a successor just like preemption and
    /// yield. Keeping their tail common prevents a tickless CPU from retaining
    /// the outgoing thread's budget or service deadline after the switch plan
    /// has already committed a different scheduling class.
    fn finish_owner_selection(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        decision: ScheduleDecision,
        now_ns: u64,
    ) -> ScheduleDecision {
        // Selection, lifecycle, and switch-handoff state are already committed
        // before this tail. Reporting a recoverable error would let block or
        // yield callers attempt to resume an outgoing thread that is no longer
        // current, so runtime failures beyond this boundary are fatal.
        if self
            .balance_after_schedule(cpu.as_mut(), decision.next(), now_ns)
            .is_err()
        {
            task_runtime::fatal_invariant(0x5343_0001, decision.next().as_u64() as usize);
        }
        if Self::program_local_timer(cpu.as_mut(), now_ns).is_err() {
            task_runtime::fatal_invariant(0x5343_0002, decision.next().as_u64() as usize);
        }
        decision
    }

    fn balance_after_schedule(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        next: ThreadId,
        now_ns: u64,
    ) -> Result<(), TaskError> {
        if cpu.idle() == Some(next) {
            let _requested = self.request_idle_pull(cpu.as_ref())?;
        } else {
            let _pushed = self.push_overloaded(cpu.as_mut())?;
            let _fair = self.balance_fair(cpu.as_mut(), now_ns)?;
        }
        Ok(())
    }

    fn balance_fair(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<Option<ThreadId>, TaskError> {
        if task_runtime::in_hard_irq() || !cpu.fair_balance_due(now_ns) {
            return Ok(None);
        }
        self.ensure_owner_cpu_online(&cpu)?;
        self.publish_owner_cpu_load_summary(cpu.as_mut());
        let source = cpu.owner();
        let result = if let Some(source_load) = cpu.try_runnable_summary() {
            let mut lower_load_target_seen = false;
            let mut selected_target = None;
            for (index, remote) in self.cpu_remotes.iter().enumerate() {
                let target = CpuId::new(index as u32);
                if !remote.is_online() || target == source {
                    continue;
                }
                let Some(target_summary) = remote.try_load_summary() else {
                    continue;
                };
                if target_summary.runnable_count() >= source_load {
                    continue;
                }
                lower_load_target_seen = true;
                if self
                    .select_owner_balance_candidate(
                        cpu.as_ref().get_ref(),
                        Some(target),
                        now_ns,
                        BalanceReason::FairPeriodic,
                    )
                    .is_none()
                {
                    continue;
                }
                let candidate = (target_summary.runnable_count(), target);
                if selected_target.is_none_or(|selected| candidate < selected) {
                    selected_target = Some(candidate);
                }
            }
            if let Some((_, target)) = selected_target {
                match self.transfer_owner_balance_candidate(
                    cpu.as_mut(),
                    target,
                    now_ns,
                    BalanceReason::FairPeriodic,
                )? {
                    Some(thread) => FairBalanceResult::Migrated(thread),
                    None => FairBalanceResult::Constrained,
                }
            } else if lower_load_target_seen {
                FairBalanceResult::Constrained
            } else {
                FairBalanceResult::Balanced
            }
        } else {
            FairBalanceResult::Balanced
        };
        let completion_now_ns = Self::scheduler_completion_now_ns(now_ns);
        let minimum_interval_ns = self.config.balance_interval_ns();
        match result {
            FairBalanceResult::Migrated(_) => {
                cpu.as_mut()
                    .reset_fair_balance(completion_now_ns, minimum_interval_ns);
            }
            FairBalanceResult::Balanced => {
                cpu.as_mut().backoff_fair_balance(
                    completion_now_ns,
                    minimum_interval_ns,
                    minimum_interval_ns.saturating_mul(FAIR_BALANCE_BALANCED_BACKOFF_FACTOR),
                );
            }
            FairBalanceResult::Constrained => {
                cpu.as_mut().backoff_fair_balance(
                    completion_now_ns,
                    minimum_interval_ns,
                    minimum_interval_ns.saturating_mul(FAIR_BALANCE_CONSTRAINED_BACKOFF_FACTOR),
                );
            }
        }
        Ok(result.migrated())
    }

    /// Commits one running owner either to its local queue, a migration
    /// handoff, or Deadline throttle state.
    ///
    /// Remote affinity writers use the same stable thread cell. Keeping the
    /// affinity decision, lifecycle transition, and local enqueue under this
    /// one guard is the scheduler equivalent of Linux's task/rq locking rule:
    /// an affinity update cannot invalidate a placement snapshot between
    /// observing it and clearing `CpuLocal::current`.
    fn schedule_out_owner_running(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        core: Arc<ThreadCore>,
        now_ns: u64,
        reason: EnqueueReason,
    ) -> Result<Option<CpuId>, TaskError> {
        self.ensure_owner_cpu_online(&cpu)?;
        let owner = cpu.owner();
        let mut sched = core.sched().lock();

        let migration_requested =
            sched.placement.migration_target().is_some() || !sched.affinity.contains(owner);
        if migration_requested {
            let target = sched
                .placement
                .migration_target()
                .filter(|target| {
                    *target != owner
                        && sched.affinity.contains(*target)
                        && self
                            .cpu_remotes
                            .get(target.as_usize())
                            .is_some_and(|remote| remote.is_online())
                })
                .or_else(|| self.select_allowed_online_cpu(&sched.affinity, Some(owner)))
                .ok_or(TaskError::InvalidConfiguration)?;
            sched.placement.set_migration_target(Some(target))?;
            sched.transition(&core, ThreadState::Ready)?;
            sched.placement.set_running_cpu(None)?;
            core.set_target_cpu(target);
            cpu.as_mut().clear_current();
            return Ok(Some(target));
        }

        if sched.entity.is_deadline_throttled() && !sched.pi_critical_rescue {
            if let SchedulingEntity::Deadline(deadline) = sched.entity {
                if !sched.is_pi_boosted() {
                    sched.base_entity = sched.entity;
                }
                sched.base_deadline = Some(deadline);
                sched.deadline_replenish_pending = true;
                cpu.as_mut()
                    .arm_deferred_scheduler_deadline(deadline.next_scheduler_event_ns());
            }
            sched.transition(&core, ThreadState::Blocked)?;
            sched.placement.set_running_cpu(None)?;
            cpu.as_mut().clear_current();
            return Ok(None);
        }

        if cpu.idle() == Some(core.id()) {
            sched.transition(&core, ThreadState::Ready)?;
            sched.placement.set_running_cpu(None)?;
            cpu.as_mut().clear_current();
            return Ok(None);
        }

        // Hide the outgoing dispatch while queue placement computes EEVDF
        // virtual time, but retain it until enqueue commits. A typed enqueue
        // failure can therefore restore the Running owner without publishing
        // a transient `current = None` state.
        let dispatch = cpu.as_mut().take_dispatch();
        if let Err(error) = sched.transition(&core, ThreadState::Ready) {
            if let Some(dispatch) = dispatch {
                cpu.as_mut().install_dispatch(dispatch);
            }
            return Err(error);
        }
        sched.placement.set_running_cpu(None)?;
        let enqueue =
            self.enqueue_owner_thread_locked(cpu.as_mut(), &core, &mut sched, now_ns, reason);
        let preempts_current = match enqueue {
            Ok(preempts_current) => preempts_current,
            Err(error) => {
                sched.placement.set_running_cpu(Some(owner))?;
                let rollback = sched.transition(&core, ThreadState::Running);
                if let Some(dispatch) = dispatch {
                    cpu.as_mut().install_dispatch(dispatch);
                }
                rollback?;
                return Err(error);
            }
        };
        cpu.as_mut().clear_current();
        drop(sched);
        drop(dispatch);
        self.finish_owner_enqueue(cpu, reason, preempts_current);
        Ok(None)
    }

    fn select_allowed_online_cpu(
        &self,
        affinity: &CpuSet,
        excluded: Option<CpuId>,
    ) -> Option<CpuId> {
        self.cpu_remotes
            .iter()
            .enumerate()
            .filter_map(|(index, remote)| {
                let cpu = CpuId::new(index as u32);
                (Some(cpu) != excluded && remote.is_online() && affinity.contains(cpu))
                    .then_some(cpu)
                    .and_then(|cpu| {
                        remote
                            .try_runnable_summary()
                            .map(|runnable| (runnable, cpu))
                    })
            })
            .min_by_key(|(load, cpu)| (*load, cpu.as_u32()))
            .map(|(_, cpu)| cpu)
    }

    fn validate_owner_next(
        sched: &ThreadSchedState,
        next: ThreadId,
        owner: CpuId,
        outgoing: Option<ThreadId>,
    ) -> Result<(), TaskError> {
        match sched.placement.on_cpu() {
            None => Ok(()),
            Some(executing_cpu) if outgoing == Some(next) && executing_cpu == owner => Ok(()),
            Some(_) => Err(TaskError::InvalidConfiguration),
        }
    }

    fn pick_owner_next(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
        outgoing: Option<ThreadId>,
    ) -> Result<OwnerNext, TaskError> {
        let owner = cpu.owner();
        let mut outgoing_migration_target = None;
        let mut reconciled = 0;
        let core = loop {
            let queued = {
                let fields = cpu.as_mut().fields_mut();
                let ordinary_rt_may_run = fields.rt_bandwidth.may_run(now_ns, false);
                fields
                    .run_queue
                    .pick_next_with_rt(ordinary_rt_may_run, |queued| {
                        queued.core.sched().lock().is_pi_boosted_rt_owner()
                    })
            };
            let Some(queued) = queued else {
                break cpu
                    .as_ref()
                    .get_ref()
                    .idle_core
                    .as_ref()
                    .cloned()
                    .ok_or(TaskError::NoRunnableThread)?;
            };
            let core = queued.core;
            let mut sched = core.sched().lock();
            Self::validate_owner_next(&sched, core.id(), owner, outgoing)?;
            let migration_target = if sched.placement.migration_target().is_some()
                || !sched.affinity.contains(owner)
            {
                Some(
                    sched
                        .placement
                        .migration_target()
                        .filter(|target| {
                            *target != owner
                                && sched.affinity.contains(*target)
                                && self
                                    .cpu_remotes
                                    .get(target.as_usize())
                                    .is_some_and(|remote| remote.is_online())
                        })
                        .or_else(|| self.select_allowed_online_cpu(&sched.affinity, Some(owner)))
                        .ok_or(TaskError::InvalidConfiguration)?,
                )
            } else {
                None
            };
            sched.entity = queued.entity;
            if !sched.is_pi_boosted() {
                sched.base_entity = queued.entity;
            }
            if let Some(target) = migration_target {
                let outgoing_candidate =
                    outgoing == Some(core.id()) && sched.placement.on_cpu() == Some(owner);
                if !outgoing_candidate {
                    Self::detach_owner_deadline_bandwidth_locked(&core, &mut sched, cpu.as_mut())?;
                }
                sched.placement.set_migration_target(Some(target))?;
                if sched.placement.queued_cpu() == Some(owner) {
                    sched.placement.set_queued_cpu(None)?;
                } else if !outgoing_candidate {
                    return Err(TaskError::InvalidConfiguration);
                }
                core.set_target_cpu(target);
                drop(sched);
                if outgoing_candidate {
                    outgoing_migration_target = Some(target);
                } else {
                    self.publish_owner_migration(&core, target, owner, target)?;
                }
                reconciled += 1;
                if reconciled == cpu.batch_limit() {
                    cpu.request_scheduler_work();
                    break cpu
                        .as_ref()
                        .get_ref()
                        .idle_core
                        .as_ref()
                        .cloned()
                        .ok_or(TaskError::NoRunnableThread)?;
                }
                continue;
            }
            sched.placement.set_queued_cpu(None)?;
            sched.placement.set_running_cpu(Some(owner))?;
            sched.placement.set_on_cpu(Some(owner))?;
            sched.transition(&core, ThreadState::Running)?;
            let dispatch = Self::owner_dispatch(&core, &sched, now_ns)?;
            drop(sched);
            cpu.as_mut().install_dispatch(dispatch);
            break core;
        };
        if cpu.as_ref().get_ref().idle() == Some(core.id()) {
            let mut sched = core.sched().lock();
            Self::validate_owner_next(&sched, core.id(), owner, outgoing)?;
            if sched.lifecycle.state() == ThreadState::Ready {
                sched.transition(&core, ThreadState::Running)?;
            }
            sched.placement.set_running_cpu(Some(owner))?;
            sched.placement.set_on_cpu(Some(owner))?;
            let dispatch = Self::owner_dispatch(&core, &sched, now_ns)?;
            cpu.as_mut().install_dispatch(dispatch);
        }
        cpu.as_mut().set_current_core(Arc::clone(&core));
        self.publish_owner_cpu_load_summary(cpu.as_mut());
        Ok(OwnerNext {
            core,
            outgoing_migration_target,
        })
    }

    fn stage_switch_handoff(
        mut cpu: Pin<&mut CpuLocal>,
        previous: Option<ThreadId>,
        previous_core: Option<Arc<ThreadCore>>,
        next: ThreadId,
        migration_target: Option<CpuId>,
    ) -> Result<(), TaskError> {
        match previous {
            Some(previous) if previous != next => {
                let previous_core = previous_core.ok_or(TaskError::InvalidConfiguration)?;
                if previous_core.id() != previous {
                    return Err(TaskError::InvalidConfiguration);
                }
                cpu.as_mut()
                    .stage_switch_handoff(previous_core, migration_target)
            }
            _ if migration_target.is_none() => Ok(()),
            _ => Err(TaskError::InvalidConfiguration),
        }
    }

    fn owner_switch_plan(
        previous: Option<&Arc<ThreadCore>>,
        next: &Arc<ThreadCore>,
        switch_reason: SwitchReason,
    ) -> ScheduleDecision {
        ScheduleDecision {
            previous: previous.map(|core| core.id()),
            next: next.id(),
            previous_endpoint: previous.map(|core| SwitchEndpoint::from_core(core)),
            next_endpoint: SwitchEndpoint::from_core(next),
            switch_reason,
        }
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
