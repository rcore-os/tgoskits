//! Generation-checked registry and scheduling orchestration.

use alloc::{sync::Arc, vec::Vec};
use core::{
    pin::Pin,
    ptr,
    sync::atomic::{AtomicUsize, Ordering},
};

use crate::{
    CpuId, CpuLocal, CpuRemote, CpuSet, CpuSnapshot, DeadlineAdmission, DeadlineEntity,
    DeadlineFlags, EnqueueReason, FairMode, ParkCommit, ParkPrepare, ParkToken, PiLockId,
    PiWaitToken, QueuedThread, SchedulePolicy, SchedulingClass, SchedulingEntity, SwitchReason,
    TaskError, TaskSystemConfig, ThreadCore, ThreadExtension, ThreadExtensionBorrow,
    ThreadExtensionLease, ThreadExtensionView, ThreadHandle, ThreadId, ThreadLifecycle,
    ThreadResources, ThreadRuntimeSnapshot, ThreadSpec, ThreadState, ThreadWakeHandle,
    inbox::{InboxKind, InboxMessage, PublishResult, SchedulerInbox},
    lock::{IrqTicketLock, SequenceCounter},
    reclaim::DeferredReclaimNode,
    runtime::{ExecutionContextHandle, RuntimeStatus, task_runtime},
    system::cpu::{CurrentDispatch, CurrentDispatchState, SchedulerIpiRetrySet},
};

/// Failure returned by [`TaskSystem::reap_thread_handle`].
///
/// A retryable failure returns ownership of the strong handle, keeping the
/// registry generation pinned while the caller yields and retries. A committed
/// teardown failure happens only after the registry record was removed and
/// therefore cannot be retried by handle.
#[derive(Debug, thiserror::Error)]
pub enum OwnedThreadReapError {
    /// The record could not yet be removed; the handle remains valid.
    #[error("{error}")]
    Retry {
        /// Scheduler error that prevented removal.
        error: TaskError,
        /// Original owning handle returned for retry.
        handle: ThreadHandle,
    },
    /// Registry removal committed, but an OS resource teardown failed.
    #[error("{0}")]
    Committed(TaskError),
}

impl OwnedThreadReapError {
    /// Returns the underlying scheduler error.
    pub const fn task_error(&self) -> TaskError {
        match self {
            Self::Retry { error, .. } | Self::Committed(error) => *error,
        }
    }

    /// Returns the still-valid handle when the operation can be retried.
    pub fn into_retry_handle(self) -> Option<ThreadHandle> {
        match self {
            Self::Retry { handle, .. } => Some(handle),
            Self::Committed(_) => None,
        }
    }
}

/// Complete OS-independent scheduler instance.
///
/// No instance is stored globally. A runtime owns one pinned `TaskSystem` and
/// passes explicit object references to the scheduler or exposes them through its
/// trait-FFI facade.
#[derive(Debug)]
pub struct TaskSystem {
    config: TaskSystemConfig,
    cpu_remotes: Vec<Arc<CpuRemote>>,
    scheduler_ipi_retries: Arc<SchedulerIpiRetrySet>,
    state: IrqTicketLock<TaskSystemState>,
    deferred_reclaims: SchedulerInbox,
    topology_sequence: SequenceCounter,
    online_count: AtomicUsize,
}

#[derive(Debug)]
struct TaskSystemState {
    fair_slice_ns: u64,
    online: CpuSet,
    cpus: Vec<CpuRegistration>,
    slots: Vec<ThreadSlot>,
    free_slots: Vec<u32>,
    deadline_admission: DeadlineAdmission,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BalanceReason {
    Summary,
    RtDeadlinePush,
    IdlePull,
    FairPeriodic,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FairPolicyPlacement {
    source_virtual_time: u64,
    destination_virtual_time: u64,
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
        let scheduler_ipi_retries = Arc::new(SchedulerIpiRetrySet::new(config.cpu_count()));
        let cpu_remotes = (0..config.cpu_count())
            .map(|index| {
                CpuRemote::create(
                    CpuId::new(index as u32),
                    config,
                    Arc::clone(&scheduler_ipi_retries),
                )
            })
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
            scheduler_ipi_retries,
            state: IrqTicketLock::new(TaskSystemState {
                fair_slice_ns: config.fair_slice_ns(),
                online: CpuSet::empty(config.cpu_count()),
                cpus: cpu_registrations,
                slots: Vec::new(),
                free_slots: Vec::new(),
                deadline_admission: DeadlineAdmission::new(config.deadline_cap_percent()),
            }),
            deferred_reclaims: SchedulerInbox::new(InboxKind::Reclaim),
            topology_sequence: SequenceCounter::default(),
            online_count: AtomicUsize::new(0),
        })
    }

    /// Reports whether a failed outbound scheduler doorbell requires a
    /// task/IRQ-return safe point before this CPU may sleep.
    pub(crate) fn scheduler_ipi_retry_pending(&self) -> bool {
        self.scheduler_ipi_retries.has_pending()
    }

    /// Services a bounded batch of preallocated scheduler-IPI retries.
    pub(crate) fn service_scheduler_ipi_retries(&self, limit: usize) -> Result<usize, TaskError> {
        const MAX_RETRY_BATCH: usize = 64;

        if task_runtime::in_hard_irq() {
            return Err(TaskError::UnsafeContext);
        }
        let mut targets = [CpuId::new(0); MAX_RETRY_BATCH];
        let limit = limit.min(MAX_RETRY_BATCH);
        let invalid = self
            .scheduler_ipi_retries
            .take_invalid_batch(&mut targets[..limit]);
        if invalid != 0 {
            task_runtime::fatal_invariant(0x4950_4901, targets[0].as_u32() as usize);
        }

        let count = self
            .scheduler_ipi_retries
            .take_retry_batch(&mut targets[..limit]);
        let mut attempted = 0;
        for &target in targets.iter().take(count) {
            let Some(remote) = self.cpu_remotes.get(target.as_u32() as usize) else {
                self.scheduler_ipi_retries.publish_invalid(target);
                continue;
            };
            attempted += usize::from(remote.retry_scheduler_ipi());
        }
        Ok(attempted)
    }

    /// Publishes one zero-reference resource to the task-context reaper.
    ///
    /// The pinned node supplies its own fixed reclaim function. Publication is
    /// allocation-free and does not invoke that function, so callers may use it
    /// from hard IRQ context. `data` must be an exposed allocation address
    /// numerically equal to the node address; this is checked before its
    /// intrusive membership is published.
    pub(crate) fn publish_deferred_reclaim(
        &self,
        node: Pin<&'static DeferredReclaimNode>,
        data: usize,
    ) -> PublishResult {
        if data != node.address() {
            task_runtime::fatal_invariant(0x4558_0007, data);
        }
        self.deferred_reclaims.publish(
            node.inbox(),
            InboxMessage::reclaim(ThreadId::from_parts(0, 0), 0, data),
        )
    }

    /// Reclaims at most `limit` resources in ordinary task context.
    ///
    /// The implementation additionally caps one pass at 64 callbacks so an
    /// accidental large caller limit cannot create an unbounded safe point.
    ///
    /// # Errors
    ///
    /// Returns [`TaskError::UnsafeContext`] from hard IRQ context.
    pub fn drain_deferred_reclaims(&self, limit: usize) -> Result<usize, TaskError> {
        const MAX_DRAIN_BATCH: usize = 64;

        if task_runtime::in_hard_irq() {
            return Err(TaskError::UnsafeContext);
        }
        let mut messages = [InboxMessage::EMPTY; MAX_DRAIN_BATCH];
        let batch = self
            .deferred_reclaims
            .drain(limit.min(MAX_DRAIN_BATCH), &mut messages);
        for message in messages.iter().take(batch.drained()) {
            let data = ptr::with_exposed_provenance_mut::<()>(message.payload());
            if data.is_null() {
                continue;
            }
            let node = data.cast::<DeferredReclaimNode>();
            unsafe {
                // Detachment cleared this node's inbox membership before the
                // fixed callback receives exclusive ownership of its resource.
                DeferredReclaimNode::reclaim(node, data);
            }
        }
        Ok(batch.drained())
    }

    /// Allocates one pinned CPU-local scheduler object without publishing it.
    pub fn create_cpu_local(
        &self,
        cpu: CpuId,
    ) -> Result<Pin<alloc::boxed::Box<CpuLocal>>, TaskError> {
        let remote = Arc::clone(&self.state.lock().cpu_registration(cpu)?.remote);
        Ok(CpuLocal::create(cpu, self.config, remote))
    }

    /// Returns the stable remote-publication endpoint of an online CPU.
    pub fn cpu_remote(&self, cpu: CpuId) -> Option<&CpuRemote> {
        self.cpu_remotes
            .get(cpu.as_usize())
            .map(Arc::as_ref)
            .filter(|remote| remote.is_online())
    }

    /// Completes CPU registration and publishes it in the online root domain.
    pub fn bring_cpu_online(&self, cpu: Pin<&mut CpuLocal>) -> Result<(), TaskError> {
        let id = cpu.owner();
        let mut state = self.state.lock();
        let registration = state.cpu_registration_mut(id)?;
        if registration.online || cpu.is_online() {
            return Err(TaskError::CpuAlreadyOnline(id.as_u32()));
        }
        if !Arc::ptr_eq(&registration.remote, cpu.remote()) {
            return Err(TaskError::InvalidRuntimeHandle);
        }
        self.topology_sequence.write_begin();
        registration.online = true;
        cpu.as_ref().get_ref().remote().mark_online();
        state.online.insert(id);
        let online_count = state.online_cpu_count();
        state.deadline_admission.set_online_cpus(online_count);
        self.online_count.store(online_count, Ordering::Release);
        self.topology_sequence.write_end();
        Ok(())
    }

    /// Creates a thread in the [`ThreadState::New`] state.
    ///
    /// Deadline threads are admitted immediately and therefore must cover the
    /// complete online root domain.
    pub fn create_thread(&self, spec: ThreadSpec) -> Result<ThreadHandle, TaskError> {
        let policy = spec.policy();
        policy.validate()?;
        let affinity = spec
            .affinity()
            .cloned()
            .unwrap_or_else(|| CpuSet::all(self.config.cpu_count()));
        validate_affinity(&affinity, self.config.cpu_count())?;
        let mut state = self.state.lock();
        let reservation = state.reserve_deadline(policy, &affinity)?;
        let (slot, generation) = state.allocate_thread_slot();
        let id = ThreadId::from_parts(slot, generation);
        let core = Arc::new(ThreadCore::new(id, policy));
        let entity = SchedulingEntity::new(policy, self.config.fair_slice_ns(), 0);
        let base_deadline = match entity {
            SchedulingEntity::Deadline(deadline) => Some(deadline),
            _ => None,
        };
        let (extension, resources) = spec.into_owned_parts();
        let record = ThreadRecord {
            core: Arc::clone(&core),
            lifecycle: ThreadLifecycle::new(),
            base_policy: policy,
            active_base_policy: policy,
            policy,
            policy_generation: 1,
            applied_policy_generation: 1,
            affinity,
            extension,
            resources,
            entity,
            base_deadline,
            deadline_activity: DeadlineActivity::Inactive,
            deadline_bandwidth_cpu: None,
            deadline_bandwidth_scaled: u64::try_from(reservation).unwrap_or(u64::MAX),
            deadline_zero_lag_ns: 0,
            active_deadline_reservation: reservation,
            desired_deadline_reservation: reservation,
            queued_cpu: None,
            running_cpu: None,
            on_cpu: None,
            migration_target: None,
            blocked_on: None,
            blocked_pi_waiters: 0,
            deadline_donor: None,
            pi_critical_rescue: false,
            deadline_replenish_pending: false,
            deadline_overrun_pending: false,
            exit_callback_pending: false,
            exit_callback_claimed: false,
            charged_runtime_ns: 0,
        };
        state.slots[slot as usize].record = Some(record);
        Ok(ThreadHandle { core })
    }

    /// Transitions a new or waking thread to `Ready`.
    pub fn make_ready(&self, thread: ThreadId) -> Result<(), TaskError> {
        let mut state = self.state.lock();
        let record = state.thread_record_mut(thread)?;
        let state = record.lifecycle.state();
        if state == ThreadState::Waking {
            record.entity.reset_after_wake(record.policy);
        }
        record.transition(ThreadState::Ready)
    }

    /// Installs the CPU's already-running bootstrap execution context.
    ///
    /// This operation is used before a CPU is published online and performs no
    /// context switch. The runtime must call it exactly once with an empty
    /// `CpuLocal` current slot.
    pub fn install_bootstrap_thread(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        spec: ThreadSpec,
    ) -> Result<ThreadHandle, TaskError> {
        let thread = self.create_thread(spec)?;
        let mut state = self.state.lock();
        state.cpu_registration(cpu.owner())?;
        if cpu.current().is_some() {
            return Err(TaskError::InvalidConfiguration);
        }
        let record = state.thread_record_mut(thread.id())?;
        record.transition(ThreadState::Ready)?;
        record.transition(ThreadState::Running)?;
        record.running_cpu = Some(cpu.owner());
        record.on_cpu = Some(cpu.owner());
        record.core.set_target_cpu(cpu.owner());
        cpu.as_mut().set_current(Some(thread.id()));
        cpu.as_mut().install_dispatch(CurrentDispatch::new(
            CurrentDispatchState {
                thread: thread.id(),
                policy: record.policy,
                entity: record.entity,
                deadline_donor: record.deadline_donor,
                blocks_pi_waiter: record.blocked_pi_waiters != 0,
                rt_quota_exempt: record.is_pi_boosted_rt_owner(),
                pi_critical_rescue: record.pi_critical_rescue,
                policy_generation: record.applied_policy_generation,
            },
            &record.core,
            task_runtime::monotonic_ns(),
        ));
        Self::publish_cpu_load_summary(&state, cpu.as_mut());
        Ok(thread)
    }

    /// Creates and registers a dedicated CPU idle thread before online publish.
    pub fn register_idle_thread(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        spec: ThreadSpec,
    ) -> Result<ThreadHandle, TaskError> {
        if !matches!(
            spec.policy(),
            SchedulePolicy::Fair {
                mode: crate::FairMode::Idle,
                ..
            }
        ) {
            return Err(TaskError::InvalidConfiguration);
        }
        let thread = self.create_thread(spec)?;
        self.make_ready(thread.id())?;
        let state = self.state.lock();
        state.cpu_registration(cpu.owner())?;
        cpu.as_mut().set_idle(thread.id());
        Ok(thread)
    }

    /// Enqueues a ready thread on an affinity-compatible owner CPU.
    pub fn enqueue(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        thread: ThreadId,
        now_ns: u64,
    ) -> Result<(), TaskError> {
        {
            let mut state = self.state.lock();
            self.enqueue_with_reason(
                &mut state,
                cpu.as_mut(),
                thread,
                now_ns,
                EnqueueReason::Wake,
            )?;
        }
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
        let placed_locally = {
            let mut state = self.state.lock();
            state.ensure_cpu_online(&cpu)?;
            let owner = cpu.owner();
            let affinity = state.thread_record(thread)?.affinity.clone();
            if affinity.contains(owner) {
                self.enqueue_with_reason(
                    &mut state,
                    cpu.as_mut(),
                    thread,
                    now_ns,
                    EnqueueReason::Wake,
                )?;
                true
            } else {
                let target = state
                    .select_allowed_cpu(&affinity)
                    .ok_or(TaskError::InvalidConfiguration)?;
                let core = {
                    let record = state.thread_record_mut(thread)?;
                    if record.lifecycle.state() != ThreadState::Ready {
                        return Err(TaskError::NotReady);
                    }
                    if record.queued_cpu.is_some()
                        || record.running_cpu.is_some()
                        || record.on_cpu.is_some()
                    {
                        return Err(TaskError::AlreadyQueued);
                    }
                    record.migration_target = Some(target);
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
        let mut state = self.state.lock();
        state.ensure_cpu_online(&cpu)?;
        let queued = cpu
            .as_mut()
            .fields_mut()
            .run_queue
            .dequeue(thread)
            .ok_or(TaskError::NotReady)?;
        let record = state.thread_record_mut(thread)?;
        record.entity = queued.entity;
        record.queued_cpu = None;
        Self::publish_cpu_load_summary(&state, cpu.as_mut());
        Ok(())
    }

    /// Drains a bounded batch of direct remote wakes on the owner CPU.
    pub fn drain_remote_wakes(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<RemoteWakeDrain, TaskError> {
        let mut state = self.state.lock();
        state.ensure_cpu_online(&cpu)?;
        cpu.acknowledge_scheduler_ipi();
        let (drained, pending) = {
            let fields = cpu.as_mut().fields_mut();
            let limit = fields.batch_limit();
            let remote = Arc::clone(fields.remote());
            let buffer = &mut fields.remote_wake_buffer;
            let batch = remote.remote_wake_inbox().drain(limit, buffer);
            (batch.drained(), batch.pending())
        };
        for index in 0..drained {
            let message = cpu.remote_wake_buffer[index];
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
            let wake = ThreadWakeHandle { core };
            if Self::consume_wake_locked(&mut state, &wake)? {
                let owner = cpu.owner();
                let target = state
                    .thread_record(wake.thread_id())?
                    .core
                    .target_cpu()
                    .unwrap_or(owner);
                if target == owner {
                    self.enqueue_with_reason(
                        &mut state,
                        cpu.as_mut(),
                        wake.thread_id(),
                        now_ns,
                        EnqueueReason::Wake,
                    )?;
                } else {
                    // Affinity may change after an IRQ publishes into the old
                    // target inbox. The old owner consumes the wake transition
                    // but hands the ready thread to the latest target instead
                    // of losing it on an affinity-invalid local enqueue.
                    Self::detach_deadline_bandwidth(&mut state, cpu.as_mut(), wake.thread_id())?;
                    let core = {
                        let record = state.thread_record_mut(wake.thread_id())?;
                        record.migration_target = Some(target);
                        Arc::clone(&record.core)
                    };
                    state.publish_migration_to(&core, target, owner, target)?;
                }
            }
        }
        if pending {
            cpu.request_scheduler_work();
        }
        Ok(RemoteWakeDrain { drained, pending })
    }

    /// Applies a bounded batch of owner-CPU effective-policy updates.
    pub fn drain_policy_updates(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<RemoteWakeDrain, TaskError> {
        let mut state = self.state.lock();
        state.ensure_cpu_online(&cpu)?;
        let (drained, pending) = {
            let fields = cpu.as_mut().fields_mut();
            let limit = fields.batch_limit();
            let remote = Arc::clone(fields.remote());
            let batch = remote
                .migration_inbox()
                .drain(limit, &mut fields.migration_buffer);
            (batch.drained(), batch.pending())
        };
        for index in 0..drained {
            let message = cpu.migration_buffer[index];
            if message.is_balance_request() {
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
                let _migrated = self.transfer_balance_candidate(
                    &mut state,
                    cpu.as_mut(),
                    target,
                    now_ns,
                    BalanceReason::IdlePull,
                )?;
                continue;
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
            if core.id() != message.thread_id() {
                continue;
            }
            let owner = cpu.owner();
            let source = message
                .source_cpu()
                .ok_or(TaskError::InvalidConfiguration)?;
            let target = message
                .target_cpu()
                .ok_or(TaskError::InvalidConfiguration)?;
            if source != target {
                if target == owner {
                    let latest_target = state.thread_record(core.id())?.migration_target;
                    if latest_target != Some(target) {
                        if let Some(latest_target) = latest_target {
                            // A second affinity update can overtake an already
                            // published transfer. Forward the detached message
                            // to the newest target; the embedded node is free
                            // again after this inbox batch detached it.
                            state
                                .thread_record(core.id())?
                                .core
                                .set_target_cpu(latest_target);
                            state.publish_migration_to(
                                &core,
                                latest_target,
                                owner,
                                latest_target,
                            )?;
                        }
                        continue;
                    }
                    let record = state.thread_record_mut(core.id())?;
                    if record.lifecycle.state() != ThreadState::Ready
                        || record.queued_cpu.is_some()
                        || record.running_cpu.is_some()
                        || record.on_cpu.is_some()
                    {
                        return Err(TaskError::InvalidConfiguration);
                    }
                    record.migration_target = None;
                    record.core.set_target_cpu(owner);
                    self.enqueue_with_reason(
                        &mut state,
                        cpu.as_mut(),
                        core.id(),
                        now_ns,
                        EnqueueReason::Migrated,
                    )?;
                } else if source == owner {
                    let (queued_cpu, running_cpu, lifecycle, latest_target) = {
                        let record = state.thread_record(core.id())?;
                        (
                            record.queued_cpu,
                            record.running_cpu,
                            record.lifecycle.state(),
                            record.migration_target,
                        )
                    };
                    let Some(latest_target) = latest_target else {
                        continue;
                    };
                    if queued_cpu == Some(owner) {
                        let queued = cpu
                            .as_mut()
                            .fields_mut()
                            .run_queue
                            .dequeue(core.id())
                            .ok_or(TaskError::NotReady)?;
                        Self::detach_deadline_bandwidth(&mut state, cpu.as_mut(), core.id())?;
                        let record = state.thread_record_mut(core.id())?;
                        record.entity = queued.entity;
                        record.queued_cpu = None;
                        record.core.set_target_cpu(latest_target);
                        Self::publish_cpu_load_summary(&state, cpu.as_mut());
                        state.publish_migration_to(&core, latest_target, source, latest_target)?;
                    } else if running_cpu == Some(owner) {
                        cpu.request_reschedule();
                    } else if matches!(
                        lifecycle,
                        ThreadState::New
                            | ThreadState::Parking
                            | ThreadState::Blocked
                            | ThreadState::Waking
                    ) {
                        let record = state.thread_record_mut(core.id())?;
                        record.core.set_target_cpu(latest_target);
                        record.migration_target = None;
                    } else {
                        state
                            .thread_record(core.id())?
                            .core
                            .set_target_cpu(latest_target);
                        state.publish_migration_to(&core, latest_target, source, latest_target)?;
                    }
                }
                continue;
            }
            let (queued_cpu, running_cpu, policy_generation) = match state.thread_record(core.id())
            {
                Ok(record) => (
                    record.queued_cpu,
                    record.running_cpu,
                    record.policy_generation,
                ),
                Err(TaskError::StaleThreadId) => continue,
                Err(error) => return Err(error),
            };
            if message.generation() > policy_generation {
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
                    Self::fair_policy_placement(&state, cpu.as_ref().get_ref(), core.id())?;
                let queued = cpu
                    .as_mut()
                    .fields_mut()
                    .run_queue
                    .dequeue(core.id())
                    .ok_or(TaskError::NotReady)?;
                Self::detach_deadline_bandwidth(&mut state, cpu.as_mut(), core.id())?;
                {
                    let record = state.thread_record_mut(core.id())?;
                    record.entity = queued.entity;
                    record.queued_cpu = None;
                }
                state.apply_base_policy_generation(
                    core.id(),
                    message.generation(),
                    self.config.fair_slice_ns(),
                    now_ns,
                    fair_placement,
                    true,
                )?;
                let entity = state.refresh_effective_entity(
                    core.id(),
                    self.config.fair_slice_ns(),
                    now_ns,
                )?;
                state.thread_record_mut(core.id())?.entity = entity;
                self.enqueue_with_reason(
                    &mut state,
                    cpu.as_mut(),
                    core.id(),
                    now_ns,
                    EnqueueReason::PolicyChanged,
                )?;
                cpu.request_reschedule();
            } else if running_cpu == Some(owner) && cpu.current() == Some(core.id()) {
                Self::commit_current_dispatch(&mut state, cpu.as_mut(), now_ns)?;
                let fair_placement =
                    Self::fair_policy_placement(&state, cpu.as_ref().get_ref(), core.id())?;
                Self::detach_deadline_bandwidth(&mut state, cpu.as_mut(), core.id())?;
                state.apply_base_policy_generation(
                    core.id(),
                    message.generation(),
                    self.config.fair_slice_ns(),
                    now_ns,
                    fair_placement,
                    true,
                )?;
                let entity = state.refresh_effective_entity(
                    core.id(),
                    self.config.fair_slice_ns(),
                    now_ns,
                )?;
                state.thread_record_mut(core.id())?.entity = entity;
                Self::activate_deadline_bandwidth(&mut state, cpu.as_mut(), core.id())?;
                let record = state.thread_record_mut(core.id())?;
                cpu.as_mut().install_dispatch(CurrentDispatch::new(
                    CurrentDispatchState {
                        thread: core.id(),
                        policy: record.policy,
                        entity: record.entity,
                        deadline_donor: record.deadline_donor,
                        blocks_pi_waiter: record.blocked_pi_waiters != 0,
                        rt_quota_exempt: record.is_pi_boosted_rt_owner(),
                        pi_critical_rescue: record.pi_critical_rescue,
                        policy_generation: record.applied_policy_generation,
                    },
                    &record.core,
                    now_ns,
                ));
                Self::publish_cpu_load_summary(&state, cpu.as_mut());
                cpu.request_reschedule();
            } else {
                if state.thread_record(core.id())?.deadline_bandwidth_cpu == Some(owner) {
                    Self::detach_deadline_bandwidth(&mut state, cpu.as_mut(), core.id())?;
                }
                state.apply_base_policy_generation(
                    core.id(),
                    message.generation(),
                    self.config.fair_slice_ns(),
                    now_ns,
                    None,
                    false,
                )?;
                Self::assign_inactive_deadline_bandwidth(&mut state, cpu.as_mut(), core.id())?;
            }
        }
        if pending {
            cpu.request_scheduler_work();
        }
        Ok(RemoteWakeDrain { drained, pending })
    }

    fn fair_policy_placement(
        state: &TaskSystemState,
        cpu: &CpuLocal,
        thread: ThreadId,
    ) -> Result<Option<FairPolicyPlacement>, TaskError> {
        let record = state.thread_record(thread)?;
        let destination_mode = match record.base_policy {
            SchedulePolicy::Fair { mode, .. } => mode,
            _ => return Ok(None),
        };
        let source_mode = record
            .entity
            .fair()
            .map_or(destination_mode, |fair| fair.mode());
        Ok(Some(FairPolicyPlacement {
            source_virtual_time: cpu.run_queue.virtual_time_for_mode(source_mode),
            destination_virtual_time: cpu.run_queue.virtual_time_for_mode(destination_mode),
        }))
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
        if task_runtime::in_hard_irq() {
            return Ok(false);
        }
        let state = self.state.lock();
        state.ensure_cpu_online(&cpu)?;
        if cpu.runnable_summary() != 0 {
            return Ok(false);
        }
        let now_ns = task_runtime::monotonic_ns();
        let target = cpu.owner();
        let source = state
            .cpus
            .iter()
            .enumerate()
            .filter(|(index, registration)| {
                registration.online && CpuId::new(*index as u32) != target
            })
            .filter_map(|(index, _)| {
                let source = CpuId::new(index as u32);
                let local = state.cpu_remote(source)?;
                let summary = local.load_summary();
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
        let source_local = state
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
        if task_runtime::in_hard_irq() {
            return Ok(None);
        }
        let mut state = self.state.lock();
        state.ensure_cpu_online(&cpu)?;
        let source = cpu.owner();
        Self::publish_cpu_load_summary(&state, cpu.as_mut());
        let source_summary = cpu.load_summary();
        if !source_summary.is_overloaded()
            || !matches!(
                source_summary.pushable_class(),
                Some(SchedulingClass::Deadline | SchedulingClass::Realtime)
            )
        {
            return Ok(None);
        }
        let target = state
            .cpus
            .iter()
            .enumerate()
            .filter(|(index, registration)| {
                registration.online && CpuId::new(*index as u32) != source
            })
            .filter_map(|(index, _)| {
                let target = CpuId::new(index as u32);
                let target_summary = state.cpu_remote(target)?.load_summary();
                if target_summary.runnable_count() >= source_summary.runnable_count() {
                    return None;
                }
                let candidate = Self::select_balance_candidate(
                    &state,
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
        self.transfer_balance_candidate(
            &mut state,
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
        let mut state = self.state.lock();
        let record = state.thread_record_mut(thread)?;
        let mut deadline = record.base_deadline.ok_or(TaskError::NotReady)?;
        deadline.replenish(now_ns);
        if deadline.is_throttled() {
            return Err(TaskError::NotReady);
        }
        match record.lifecycle.state() {
            ThreadState::Blocked => {
                record.transition(ThreadState::Waking)?;
                record.transition(ThreadState::Ready)?;
            }
            ThreadState::Waking => record.transition(ThreadState::Ready)?,
            ThreadState::Ready => {}
            _ => return Err(TaskError::NotReady),
        }
        record.base_deadline = Some(deadline);
        record.entity = SchedulingEntity::Deadline(deadline);
        record.deadline_replenish_pending = false;
        self.enqueue_with_reason(
            &mut state,
            cpu.as_mut(),
            thread,
            now_ns,
            EnqueueReason::Replenished,
        )?;
        drop(state);
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
        self.state.lock().ensure_cpu_online(&cpu)?;
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
        self.complete_context_switch(cpu.as_mut())?;
        self.drain_owner_work(cpu.as_mut(), now_ns)?;
        let decision = {
            let mut state = self.state.lock();
            state.ensure_cpu_online(&cpu)?;
            cpu.as_mut().scheduler_enter();
            Self::commit_current_dispatch(&mut state, cpu.as_mut(), now_ns)?;
            self.service_deadline_timers(&mut state, cpu.as_mut(), now_ns)?;
            let previous = cpu.current();
            let mut migration_target = None;
            if let Some(previous) = previous {
                if state.thread_record(previous)?.migration_target.is_some() {
                    migration_target =
                        Some(self.migrate_running(&mut state, cpu.as_mut(), previous)?);
                } else {
                    self.requeue_running(
                        &mut state,
                        cpu.as_mut(),
                        previous,
                        now_ns,
                        EnqueueReason::Preempted,
                    )?;
                }
            }
            let next = Self::pick_next(&mut state, cpu.as_mut(), now_ns, previous)?;
            Self::stage_switch_handoff(cpu.as_mut(), previous, next, migration_target)?;
            let reason = if migration_target.is_some() {
                SwitchReason::Migrated
            } else {
                SwitchReason::Preempted
            };
            state.switch_plan(previous, next, reason)
        };
        Self::program_local_timer(cpu.as_mut(), now_ns)?;
        self.balance_after_schedule(cpu.as_mut(), decision.next(), now_ns)?;
        Ok(decision)
    }

    /// Services sticky scheduler work and switches only for a real preemption.
    pub fn schedule_if_requested(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<SchedulerOutcome, TaskError> {
        self.complete_context_switch(cpu.as_mut())?;
        self.drain_owner_work(cpu.as_mut(), now_ns)?;
        let mut state = self.state.lock();
        state.ensure_cpu_online(&cpu)?;
        if let Some(current) = cpu.current()
            && state.thread_record(current)?.lifecycle.state() == ThreadState::Parking
        {
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
        Self::commit_current_dispatch(&mut state, cpu.as_mut(), now_ns)?;
        self.service_deadline_timers(&mut state, cpu.as_mut(), now_ns)?;
        // Work published while this bounded safe point is running must affect
        // this decision. `scheduler_enter` consumes only the request observed
        // on entry; the second exchange closes the publication window without
        // losing a request that races after it.
        switch_requested |= cpu.take_preempt_requested();
        let previous = cpu.current();
        if let Some(previous) = previous
            && !switch_requested
        {
            let record = state.thread_record(previous)?;
            cpu.as_mut().install_dispatch(CurrentDispatch::new(
                CurrentDispatchState {
                    thread: previous,
                    policy: record.policy,
                    entity: record.entity,
                    deadline_donor: record.deadline_donor,
                    blocks_pi_waiter: record.blocked_pi_waiters != 0,
                    rt_quota_exempt: record.is_pi_boosted_rt_owner(),
                    pi_critical_rescue: record.pi_critical_rescue,
                    policy_generation: record.applied_policy_generation,
                },
                &record.core,
                now_ns,
            ));
            Self::publish_cpu_load_summary(&state, cpu.as_mut());
            // `scheduler_enter` consumed the sticky entry request, but a
            // bounded inbox drain may have left another batch behind. Preserve
            // that work (and any request produced by Deadline servicing) for
            // the next scheduler safe point.
            if cpu.has_remote_work() {
                cpu.request_scheduler_work();
            }
            drop(state);
            Self::program_local_timer(cpu.as_mut(), now_ns)?;
            return Ok(if cpu.has_remote_work() {
                SchedulerOutcome::OwnerWorkPending
            } else {
                SchedulerOutcome::Quiescent
            });
        }
        let mut migration_target = None;
        if let Some(previous) = previous {
            if state.thread_record(previous)?.migration_target.is_some() {
                migration_target =
                    Some(self.migrate_running(&mut state, cpu.as_mut(), previous)?);
            } else {
                self.requeue_running(
                    &mut state,
                    cpu.as_mut(),
                    previous,
                    now_ns,
                    EnqueueReason::Preempted,
                )?;
            }
        }
        let next = Self::pick_next(&mut state, cpu.as_mut(), now_ns, previous)?;
        Self::stage_switch_handoff(cpu.as_mut(), previous, next, migration_target)?;
        let reason = if migration_target.is_some() {
            SwitchReason::Migrated
        } else {
            SwitchReason::Preempted
        };
        let decision = state.switch_plan(previous, next, reason);
        drop(state);
        Self::program_local_timer(cpu.as_mut(), now_ns)?;
        self.balance_after_schedule(cpu.as_mut(), decision.next(), now_ns)?;
        Ok(SchedulerOutcome::Decision(decision))
    }

    /// Moves the current thread to its class tail and selects another thread.
    pub fn yield_current(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<ScheduleDecision, TaskError> {
        self.complete_context_switch(cpu.as_mut())?;
        self.drain_owner_work(cpu.as_mut(), now_ns)?;
        let mut state = self.state.lock();
        state.ensure_cpu_online(&cpu)?;
        cpu.as_mut().scheduler_enter();
        Self::commit_current_dispatch(&mut state, cpu.as_mut(), now_ns)?;
        self.service_deadline_timers(&mut state, cpu.as_mut(), now_ns)?;
        let previous = cpu.current();
        let mut migration_target = None;
        if let Some(previous) = previous {
            let deadline_job_ended = {
                let record = state.thread_record_mut(previous)?;
                if matches!(record.active_base_policy, SchedulePolicy::Deadline(_))
                    && record.deadline_donor.is_none()
                {
                    if !record.entity.yield_deadline_job() {
                        return Err(TaskError::InvalidConfiguration);
                    }
                    if let SchedulingEntity::Deadline(deadline) = record.entity {
                        record.base_deadline = Some(deadline);
                        cpu.as_mut()
                            .arm_deferred_scheduler_deadline(deadline.next_scheduler_event_ns());
                    }
                    record.running_cpu = None;
                    record.deadline_replenish_pending = true;
                    record.transition(ThreadState::Blocked)?;
                    true
                } else {
                    false
                }
            };
            if deadline_job_ended {
                Self::mark_deadline_non_contending(&mut state, cpu.as_mut(), previous, now_ns)?;
                cpu.as_mut().set_current(None);
            } else if state.thread_record(previous)?.migration_target.is_some() {
                migration_target =
                    Some(self.migrate_running(&mut state, cpu.as_mut(), previous)?);
            } else {
                self.requeue_running(
                    &mut state,
                    cpu.as_mut(),
                    previous,
                    now_ns,
                    EnqueueReason::Yield,
                )?;
            }
        }
        let next = Self::pick_next(&mut state, cpu.as_mut(), now_ns, previous)?;
        Self::stage_switch_handoff(cpu.as_mut(), previous, next, migration_target)?;
        let decision = state.switch_plan(previous, next, SwitchReason::Yield);
        drop(state);
        Self::program_local_timer(cpu.as_mut(), now_ns)?;
        self.balance_after_schedule(cpu.as_mut(), decision.next(), now_ns)?;
        Ok(decision)
    }

    /// Publishes `PARKING` after consuming a wake-before-park notification.
    pub fn prepare_park(&self, mut cpu: Pin<&mut CpuLocal>) -> Result<ParkPrepare, TaskError> {
        self.complete_context_switch(cpu.as_mut())?;
        let mut state = self.state.lock();
        state.ensure_cpu_online(&cpu)?;
        let thread = cpu.current().ok_or(TaskError::NoRunnableThread)?;
        let record = state.thread_record_mut(thread)?;
        if record.core.take_park_notification() {
            return Ok(ParkPrepare::Notified);
        }
        let generation = record.core.next_park_generation();
        record.transition(ThreadState::Parking)?;
        Ok(ParkPrepare::Prepared(ParkToken::new(thread, generation)))
    }

    /// Rechecks a prepared park and either cancels it or commits schedule-out.
    pub fn commit_park(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        token: ParkToken,
    ) -> Result<ParkCommit, TaskError> {
        let now_ns = task_runtime::monotonic_ns();
        self.drain_owner_work(cpu.as_mut(), now_ns)?;
        let mut state = self.state.lock();
        state.ensure_cpu_online(&cpu)?;
        if cpu.current() != Some(token.thread()) {
            return Err(TaskError::StaleThreadId);
        }
        let generation = state.thread_record(token.thread())?.core.park_generation();
        if generation != token.generation() {
            return Err(TaskError::StaleThreadId);
        }
        let notified = state
            .thread_record(token.thread())?
            .core
            .take_park_notification();
        if notified {
            let record = state.thread_record_mut(token.thread())?;
            record.transition(ThreadState::Running)?;
            cpu.finish_park_preemption(true);
            return Ok(ParkCommit::Notified);
        }
        cpu.as_mut().scheduler_enter();
        cpu.finish_park_preemption(false);
        Self::commit_current_dispatch(&mut state, cpu.as_mut(), now_ns)?;
        let record = state.thread_record_mut(token.thread())?;
        record.transition(ThreadState::Blocked)?;
        record.running_cpu = None;
        Self::mark_deadline_non_contending(&mut state, cpu.as_mut(), token.thread(), now_ns)?;
        cpu.as_mut().set_current(None);
        let next = Self::pick_next(&mut state, cpu.as_mut(), now_ns, Some(token.thread()))?;
        Self::stage_switch_handoff(cpu.as_mut(), Some(token.thread()), next, None)?;
        Ok(ParkCommit::Blocked(state.switch_plan(
            Some(token.thread()),
            next,
            SwitchReason::Blocked,
        )))
    }

    /// Cancels a prepared park because an independent grant won the race.
    pub fn cancel_park(&self, cpu: Pin<&mut CpuLocal>, token: ParkToken) -> Result<(), TaskError> {
        let mut state = self.state.lock();
        state.ensure_cpu_online(&cpu)?;
        if cpu.current() != Some(token.thread()) {
            return Err(TaskError::StaleThreadId);
        }
        let record = state.thread_record_mut(token.thread())?;
        if record.core.park_generation() != token.generation() {
            return Err(TaskError::StaleThreadId);
        }
        record.transition(ThreadState::Running)?;
        cpu.finish_park_preemption(true);
        Ok(())
    }

    /// Parks the current thread and selects its replacement.
    pub fn block_current(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
    ) -> Result<ScheduleDecision, TaskError> {
        match self.prepare_park(cpu.as_mut())? {
            ParkPrepare::Prepared(token) => match self.commit_park(cpu.as_mut(), token)? {
                ParkCommit::Blocked(decision) => Ok(decision),
                ParkCommit::Notified => {
                    let state = self.state.lock();
                    Ok(state.switch_plan(
                        Some(token.thread()),
                        token.thread(),
                        SwitchReason::Blocked,
                    ))
                }
            },
            ParkPrepare::Notified => {
                let current = cpu.current().ok_or(TaskError::NoRunnableThread)?;
                let state = self.state.lock();
                Ok(state.switch_plan(Some(current), current, SwitchReason::Blocked))
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
        self.complete_context_switch(cpu.as_mut())?;
        self.drain_owner_work(cpu.as_mut(), now_ns)?;
        let state = self.state.lock();
        state.ensure_cpu_online(&cpu)?;
        let current = cpu.current().ok_or(TaskError::NoRunnableThread)?;
        if cpu.idle() == Some(current) {
            return Err(TaskError::InvalidConfiguration);
        }
        let record = state.thread_record(current)?;
        let lifecycle = record.lifecycle.state();
        if lifecycle != ThreadState::Running {
            return Err(TaskError::InvalidTransition {
                from: lifecycle,
                to: ThreadState::Exited,
            });
        }
        if record.has_live_pi_edges() {
            return Err(TaskError::InvalidPiState);
        }
        if record.running_cpu != Some(cpu.owner()) || record.on_cpu != Some(cpu.owner()) {
            return Err(TaskError::ThreadBusy);
        }
        if record.resources.context().is_none() {
            return Err(TaskError::InvalidRuntimeHandle);
        }
        Ok(current)
    }

    /// Commits current-thread exit and selects a replacement.
    pub fn exit_current(&self, mut cpu: Pin<&mut CpuLocal>) -> Result<ScheduleDecision, TaskError> {
        self.complete_context_switch(cpu.as_mut())?;
        let now_ns = task_runtime::monotonic_ns();
        self.drain_owner_work(cpu.as_mut(), now_ns)?;
        let decision = {
            let mut state = self.state.lock();
            state.ensure_cpu_online(&cpu)?;
            let previous = cpu.current().ok_or(TaskError::NoRunnableThread)?;
            if state.thread_record(previous)?.has_live_pi_edges() {
                return Err(TaskError::InvalidPiState);
            }
            cpu.as_mut().scheduler_enter();
            Self::commit_current_dispatch(&mut state, cpu.as_mut(), now_ns)?;
            Self::detach_deadline_bandwidth(&mut state, cpu.as_mut(), previous)?;
            {
                let record = state.thread_record_mut(previous)?;
                record.transition(ThreadState::Exited)?;
                record.running_cpu = None;
                record.exit_callback_pending = record.extension.is_some();
                record.exit_callback_claimed = false;
            }
            state.release_deadline_reservation_on_exit(previous)?;
            cpu.as_mut().set_current(None);
            let next = Self::pick_next(&mut state, cpu.as_mut(), now_ns, Some(previous))?;
            Self::stage_switch_handoff(cpu.as_mut(), Some(previous), next, None)?;
            state.switch_plan(Some(previous), next, SwitchReason::Exited)
        };
        Ok(decision)
    }

    /// Completes the physical switch-out handoff in the newly active context.
    ///
    /// This second phase clears `on_cpu` only after architecture execution has
    /// left the previous stack. Deferred migration publication and exit hooks
    /// therefore cannot make a context runnable or reapable too early.
    pub fn complete_context_switch(&self, mut cpu: Pin<&mut CpuLocal>) -> Result<(), TaskError> {
        let Some(handoff) = cpu.as_mut().take_switch_handoff() else {
            return Ok(());
        };
        {
            let mut state = self.state.lock();
            let migration = {
                let record = state.thread_record_mut(handoff.previous)?;
                if record.on_cpu != Some(cpu.owner()) {
                    return Err(TaskError::InvalidConfiguration);
                }
                record.on_cpu = None;
                match handoff.migration_target {
                    Some(_) => {
                        let target = record
                            .migration_target
                            .ok_or(TaskError::InvalidConfiguration)?;
                        if record.lifecycle.state() != ThreadState::Ready
                            || record.queued_cpu.is_some()
                            || record.running_cpu.is_some()
                        {
                            return Err(TaskError::InvalidConfiguration);
                        }
                        record.core.set_target_cpu(target);
                        Some((Arc::clone(&record.core), target))
                    }
                    None => None,
                }
            };
            if let Some((core, target)) = migration {
                Self::detach_deadline_bandwidth(&mut state, cpu.as_mut(), handoff.previous)?;
                state.publish_migration_to(&core, target, cpu.owner(), target)?;
            }
            Self::publish_cpu_load_summary(&state, cpu.as_mut());
        }
        Ok(())
    }

    /// Consumes a direct wake publication and changes a blocked thread to ready.
    pub fn consume_wake(&self, wake: &ThreadWakeHandle) -> Result<bool, TaskError> {
        let mut state = self.state.lock();
        Self::consume_wake_locked(&mut state, wake)
    }

    fn consume_wake_locked(
        state: &mut TaskSystemState,
        wake: &ThreadWakeHandle,
    ) -> Result<bool, TaskError> {
        let record = match state.thread_record_mut(wake.thread_id()) {
            Ok(record) => record,
            // A late IRQ wake racing with reaping or slot reuse is an idempotent
            // no-op, not a registry lookup failure visible to the IRQ producer.
            Err(TaskError::StaleThreadId) => return Ok(false),
            Err(error) => return Err(error),
        };
        let lifecycle = record.lifecycle.state();
        if !record.core.consume_wake(lifecycle == ThreadState::Parking)
            || lifecycle == ThreadState::Exited
        {
            return Ok(false);
        }
        if record.deadline_replenish_pending {
            return Ok(false);
        }
        match lifecycle {
            ThreadState::Parking => {
                // PARKING still executes on this CPU and remains `current` /
                // `on_cpu`. The wake's park notification is the ownership
                // handoff to `commit_park`; enqueueing here would make the same
                // context both running and runnable.
                Ok(false)
            }
            ThreadState::Blocked => {
                record.transition(ThreadState::Waking)?;
                record.entity.reset_after_wake(record.policy);
                record.transition(ThreadState::Ready)?;
                Ok(true)
            }
            ThreadState::Ready | ThreadState::Running | ThreadState::Waking => Ok(false),
            ThreadState::New | ThreadState::Exited => Ok(false),
        }
    }

    /// Changes thread affinity after validating Deadline root-domain coverage.
    pub fn set_affinity(&self, thread: ThreadId, affinity: CpuSet) -> Result<(), TaskError> {
        validate_affinity(&affinity, self.config.cpu_count())?;
        let mut state = self.state.lock();
        let record = state.thread_record(thread)?;
        let is_deadline = matches!(record.active_base_policy, SchedulePolicy::Deadline(_))
            || matches!(record.base_policy, SchedulePolicy::Deadline(_));
        if is_deadline && !affinity.covers(&state.online) {
            return Err(TaskError::DeadlineAffinity);
        }
        let timer_cpu = state.thread_record(thread)?.core.sleep_timer_cpu();
        if timer_cpu.is_some_and(|cpu| !affinity.contains(cpu)) {
            return Err(TaskError::ActiveTimerAffinity);
        }
        let target = timer_cpu
            .or_else(|| state.select_allowed_cpu(&affinity))
            .ok_or(TaskError::InvalidConfiguration)?;
        let (source, core) = {
            let record = state.thread_record_mut(thread)?;
            record.affinity = affinity;
            let location = record.running_cpu.or(record.queued_cpu);
            let source = match location {
                Some(owner) if !record.affinity.contains(owner) => {
                    record.migration_target = Some(target);
                    Some(owner)
                }
                Some(owner) => {
                    // A newer mask made the owner legal again before its
                    // pending migration request ran. Cancel that request.
                    record.migration_target = None;
                    record.core.set_target_cpu(owner);
                    None
                }
                None if record.migration_target.is_some() => {
                    // The source already detached this ready thread and a
                    // transfer is in flight. Retarget the transfer in-place;
                    // the old destination forwards it after observing this
                    // state under the scheduler lock.
                    record.migration_target = Some(target);
                    record.core.set_target_cpu(target);
                    None
                }
                None => {
                    record.core.set_target_cpu(target);
                    None
                }
            };
            (source, Arc::clone(&record.core))
        };
        if let Some(source) = source {
            state.publish_migration_request(&core, source, target)?;
        } else if state
            .thread_record(thread)?
            .running_cpu
            .or(state.thread_record(thread)?.queued_cpu)
            .is_some()
        {
            // Affinity can change generic pushability without moving the
            // thread. Let the owner refresh its epoch-protected load summary;
            // a stale idle-pull request is still decided from registry state.
            state.request_owner_reschedule(thread);
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
        validate_affinity(&affinity, self.config.cpu_count())?;
        let mut state = self.state.lock();
        state.ensure_cpu_online(&cpu)?;
        let current = cpu.current().ok_or(TaskError::NoRunnableThread)?;
        let record = state.thread_record(current)?;
        if record.running_cpu != Some(cpu.owner()) || record.on_cpu != Some(cpu.owner()) {
            return Err(TaskError::InvalidConfiguration);
        }
        let is_deadline = matches!(record.active_base_policy, SchedulePolicy::Deadline(_))
            || matches!(record.base_policy, SchedulePolicy::Deadline(_));
        if is_deadline && !affinity.covers(&state.online) {
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
        let record = state.thread_record_mut(current)?;
        record.affinity = affinity;
        record.migration_target = must_migrate.then_some(target);
        record
            .core
            .set_target_cpu(if must_migrate { target } else { owner });
        if must_migrate {
            cpu.request_reschedule();
        }
        Self::publish_cpu_load_summary(&state, cpu.as_mut());
        Ok(must_migrate)
    }

    /// Installs an idle thread for a CPU; idle is selected only when queues empty.
    pub fn install_idle_thread(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        thread: ThreadId,
    ) -> Result<(), TaskError> {
        let state = self.state.lock();
        state.cpu_registration(cpu.owner())?;
        state.thread_record(thread)?;
        cpu.as_mut().set_idle(thread);
        Ok(())
    }

    /// Marks a non-queued thread exited and invokes its task-context exit hook.
    pub fn mark_exited(&self, thread: ThreadId) -> Result<(), TaskError> {
        let extension = {
            let mut state = self.state.lock();
            let extension = {
                let record = state.thread_record_mut(thread)?;
                if record.queued_cpu.is_some() || record.running_cpu.is_some() {
                    return Err(TaskError::AlreadyQueued);
                }
                if record.on_cpu.is_some() {
                    return Err(TaskError::ThreadBusy);
                }
                if record.has_live_pi_edges() {
                    return Err(TaskError::InvalidPiState);
                }
                record.transition(ThreadState::Exited)?;
                record.exit_callback_pending = record.extension.is_some();
                record.exit_callback_claimed = record.exit_callback_pending;
                record.extension.as_ref().map(ThreadExtension::as_view)
            };
            state.release_deadline_reservation_on_exit(thread)?;
            extension
        };
        if let Some(extension) = extension {
            // SAFETY: ThreadExtension::new requires the OS to keep `data` valid
            // for this callback table until the reaper invokes `drop`.
            unsafe { (extension.ops().on_exit)(extension.data(), thread) };
            let mut state = self.state.lock();
            let record = state.thread_record_mut(thread)?;
            if !record.exit_callback_pending
                || !record.exit_callback_claimed
                || record.on_cpu.is_some()
            {
                return Err(TaskError::InvalidConfiguration);
            }
            record.exit_callback_pending = false;
            record.exit_callback_claimed = false;
        }
        Ok(())
    }

    /// Runs pending exit callbacks from an ordinary task-context safe point.
    ///
    /// Context-switch tail only proves that the exited stack is inactive; its
    /// inherited IRQ and scheduler guards are still live. Calling an OS exit
    /// hook there can acquire a sleepable lock and recursively enter the
    /// scheduler. This bounded pass claims each callback under the registry
    /// lock, invokes it without scheduler locks, and only then makes the record
    /// eligible for reaping.
    pub fn dispatch_exit_callbacks(&self, limit: usize) -> Result<usize, TaskError> {
        if task_runtime::in_hard_irq() {
            return Err(TaskError::UnsafeContext);
        }
        let mut dispatched = 0;
        while dispatched < limit {
            let callback = {
                let mut state = self.state.lock();
                state.claim_pending_exit_callback()?
            };
            let Some((extension, thread)) = callback else {
                break;
            };
            // SAFETY: the registry record keeps the claimed extension live,
            // and ThreadExtension construction validated this callback table.
            unsafe { (extension.ops().on_exit)(extension.data(), thread) };
            self.state.lock().finish_exit_callback(thread)?;
            dispatched += 1;
        }
        Ok(dispatched)
    }

    /// Removes an exited registry record and makes its slot reusable.
    pub fn reap_thread(&self, thread: ThreadId) -> Result<(), TaskError> {
        let record = {
            let mut state = self.state.lock();
            state.remove_exited_thread(thread)?
        };
        release_thread_record(record)
    }

    /// Atomically removes an exited thread while consuming its owning handle.
    ///
    /// Keeping `handle` alive until registry removal prevents the detached
    /// reaper on another CPU from winning between a handle drop and an ID-based
    /// reap. Retryable failures return the same handle to the caller.
    pub fn reap_thread_handle(&self, handle: ThreadHandle) -> Result<(), OwnedThreadReapError> {
        if task_runtime::in_hard_irq() {
            return Err(OwnedThreadReapError::Retry {
                error: TaskError::UnsafeContext,
                handle,
            });
        }
        let record = {
            let mut state = self.state.lock();
            match state.remove_exited_thread_with_handle(&handle) {
                Ok(record) => record,
                Err(error) => return Err(OwnedThreadReapError::Retry { error, handle }),
            }
        };
        drop(handle);
        release_thread_record(record).map_err(OwnedThreadReapError::Committed)
    }

    /// Reaps exited records for which no external strong handle remains.
    ///
    /// This bounded task-context pass is the detached-thread reaper. Joinable
    /// threads remain registered because their [`ThreadHandle`] contributes a
    /// strong reference. Late IRQ wake handles likewise delay resource release
    /// until their final reference reaches the task-context reaper.
    pub fn reap_unreferenced_exited(&self, limit: usize) -> Result<usize, TaskError> {
        if task_runtime::in_hard_irq() {
            return Err(TaskError::UnsafeContext);
        }
        let mut reaped = 0;
        while reaped < limit {
            let record = {
                let mut state = self.state.lock();
                state.take_unreferenced_exited()?
            };
            let Some(record) = record else {
                break;
            };
            release_thread_record(record)?;
            reaped += 1;
        }
        Ok(reaped)
    }

    /// Returns the current state of a live registry entry.
    pub fn thread_state(&self, thread: ThreadId) -> Result<ThreadState, TaskError> {
        Ok(self.state.lock().thread_record(thread)?.lifecycle.state())
    }

    /// Returns cumulative charged CPU runtime at `now_ns`.
    ///
    /// The thread header uses a lock-free sequence snapshot, so a running
    /// thread includes time since its last timer or scheduler accounting point.
    pub fn thread_runtime(
        &self,
        thread: ThreadId,
        now_ns: u64,
    ) -> Result<ThreadRuntimeSnapshot, TaskError> {
        let state = self.state.lock();
        let record = state.thread_record(thread)?;
        let snapshot = record.core.runtime_snapshot(now_ns);
        debug_assert!(snapshot.charged_runtime_ns() >= record.charged_runtime_ns);
        Ok(snapshot)
    }

    /// Replaces the current running thread's opaque address-space token.
    ///
    /// The caller must hold the owner CPU's IRQ-off scheduler-safe window. This
    /// operation updates only scheduler metadata; installing the hardware page
    /// table and invalidating translations remain runtime responsibilities.
    pub fn replace_current_address_space(
        &self,
        cpu: Pin<&mut CpuLocal>,
        address_space: crate::runtime::AddressSpaceHandle,
    ) -> Result<crate::runtime::AddressSpaceHandle, TaskError> {
        if address_space.is_none() {
            return Err(TaskError::InvalidConfiguration);
        }
        let mut state = self.state.lock();
        state.ensure_cpu_online(&cpu)?;
        let owner = cpu.owner();
        let current = cpu.current().ok_or(TaskError::NoRunnableThread)?;
        let record = state.thread_record_mut(current)?;
        if record.lifecycle.state() != ThreadState::Running
            || record.running_cpu != Some(owner)
            || record.on_cpu != Some(owner)
            || record.queued_cpu.is_some()
        {
            return Err(TaskError::InvalidConfiguration);
        }
        Ok(record.resources.replace_address_space(address_space))
    }

    /// Attempts a non-waiting state query.
    ///
    /// Returns `Ok(None)` when another CPU owns the registry critical section.
    pub fn try_thread_state(&self, thread: ThreadId) -> Result<Option<ThreadState>, TaskError> {
        let Some(state) = self.state.try_lock() else {
            return Ok(None);
        };
        Ok(Some(state.thread_record(thread)?.lifecycle.state()))
    }

    /// Acquires a strong handle for a generation-valid registry entry.
    pub fn thread_handle(&self, thread: ThreadId) -> Result<ThreadHandle, TaskError> {
        let state = self.state.lock();
        let record = state.thread_record(thread)?;
        Ok(ThreadHandle {
            core: Arc::clone(&record.core),
        })
    }

    /// Borrows the opaque OS extension through a generation-valid strong handle.
    ///
    /// The borrow cannot outlive `handle`, which prevents the registry reaper
    /// from releasing the extension data while a caller interprets it.
    pub fn thread_extension<'thread>(
        &self,
        handle: &'thread ThreadHandle,
    ) -> Result<Option<ThreadExtensionBorrow<'thread>>, TaskError> {
        let view = self.thread_extension_view(handle)?;
        Ok(view.map(|view| ThreadExtensionBorrow::new(view, handle)))
    }

    /// Acquires an owned lease for callers that looked up a temporary handle.
    pub fn thread_extension_lease(
        &self,
        handle: ThreadHandle,
    ) -> Result<Option<ThreadExtensionLease>, TaskError> {
        let view = self.thread_extension_view(&handle)?;
        Ok(view.map(|view| ThreadExtensionLease::new(view, handle)))
    }

    fn thread_extension_view(
        &self,
        handle: &ThreadHandle,
    ) -> Result<Option<ThreadExtensionView>, TaskError> {
        let state = self.state.lock();
        let record = state.thread_record(handle.id())?;
        if !Arc::ptr_eq(&record.core, &handle.core) {
            return Err(TaskError::StaleThreadId);
        }
        Ok(record.extension.as_ref().map(ThreadExtension::as_view))
    }

    /// Returns the thread's effective/base scheduling policy snapshot.
    pub fn thread_policy(&self, thread: ThreadId) -> Result<SchedulePolicy, TaskError> {
        Ok(self.state.lock().thread_record(thread)?.base_policy)
    }

    /// Publishes a new base-policy generation for owner-CPU application.
    pub fn set_thread_policy(
        &self,
        thread: ThreadId,
        policy: SchedulePolicy,
    ) -> Result<(), TaskError> {
        policy.validate()?;
        let mut state = self.state.lock();
        let (active_reservation, desired_reservation, affinity, owner, core, generation) = {
            let record = state.thread_record(thread)?;
            (
                record.active_deadline_reservation,
                record.desired_deadline_reservation,
                record.affinity.clone(),
                record
                    .running_cpu
                    .or(record.queued_cpu)
                    .or(record.deadline_bandwidth_cpu),
                Arc::clone(&record.core),
                record
                    .policy_generation
                    .checked_add(1)
                    .ok_or(TaskError::InvalidConfiguration)?,
            )
        };
        let reservation = state.deadline_reservation_for(policy, &affinity)?;
        let old_held = active_reservation.max(desired_reservation);
        let new_held = active_reservation.max(reservation);
        if new_held > old_held {
            state
                .deadline_admission
                .reserve_utilization(new_held - old_held)?;
        } else {
            state.deadline_admission.release(old_held - new_held);
        }
        {
            let record = state.thread_record_mut(thread)?;
            record.desired_deadline_reservation = reservation;
            record.base_policy = policy;
            record.policy_generation = generation;
        }
        core.publish_base_policy(policy);
        if owner.is_some() {
            state.request_owner_reschedule(thread);
        } else {
            state.apply_base_policy_generation(
                thread,
                generation,
                self.config.fair_slice_ns(),
                task_runtime::monotonic_ns(),
                None,
                false,
            )?;
        }
        Ok(())
    }

    /// Returns a copy of the thread CPU affinity mask.
    pub fn thread_affinity(&self, thread: ThreadId) -> Result<CpuSet, TaskError> {
        Ok(self.state.lock().thread_record(thread)?.affinity.clone())
    }

    /// Returns the RR quantum for a round-robin thread.
    pub fn round_robin_interval_ns(&self, thread: ThreadId) -> Result<u64, TaskError> {
        match self.thread_policy(thread)? {
            SchedulePolicy::RoundRobin { quantum_ns, .. } => Ok(quantum_ns),
            _ => Err(TaskError::InvalidConfiguration),
        }
    }

    /// Returns Deadline budget and PI rescue state for diagnostics and ABI glue.
    pub fn deadline_runtime(&self, thread: ThreadId) -> Result<DeadlineRuntimeSnapshot, TaskError> {
        let state = self.state.lock();
        let record = state.thread_record(thread)?;
        let deadline = record
            .base_deadline
            .or(match record.entity {
                SchedulingEntity::Deadline(deadline) => Some(deadline),
                _ => None,
            })
            .ok_or(TaskError::InvalidConfiguration)?;
        Ok(DeadlineRuntimeSnapshot {
            remaining_runtime_ns: deadline.remaining_runtime_ns(),
            misses: deadline.misses(),
            overruns: deadline.overruns(),
            pi_critical_rescue: record.pi_critical_rescue,
            donor: record.deadline_donor,
        })
    }

    /// Returns the thread's GRUB activity, zero-lag, and runqueue ownership.
    pub fn deadline_activity(
        &self,
        thread: ThreadId,
    ) -> Result<DeadlineActivitySnapshot, TaskError> {
        let state = self.state.lock();
        let record = state.thread_record(thread)?;
        if !matches!(record.active_base_policy, SchedulePolicy::Deadline(_)) {
            return Err(TaskError::InvalidConfiguration);
        }
        Ok(DeadlineActivitySnapshot {
            activity: record.deadline_activity,
            bandwidth_cpu: record.deadline_bandwidth_cpu,
            zero_lag_ns: record.deadline_zero_lag_ns,
        })
    }

    /// Runs a bounded batch of deferred Deadline overrun callbacks.
    ///
    /// Timer IRQ only publishes pending state. This task-context operation drops
    /// the registry lock before invoking any OS extension callback.
    pub fn dispatch_deadline_overruns(&self, limit: usize) -> usize {
        let callbacks = {
            let mut state = self.state.lock();
            let mut callbacks = Vec::with_capacity(limit.min(state.slots.len()));
            for slot in &mut state.slots {
                if callbacks.len() == limit {
                    break;
                }
                let Some(record) = &mut slot.record else {
                    continue;
                };
                if !record.deadline_overrun_pending {
                    continue;
                }
                record.deadline_overrun_pending = false;
                let notify = matches!(
                    record.active_base_policy,
                    SchedulePolicy::Deadline(policy)
                        if policy.flags().contains(DeadlineFlags::DL_OVERRUN)
                );
                if notify && let Some(extension) = record.extension.as_ref() {
                    callbacks.push((
                        extension.as_view(),
                        record.core.id(),
                        Arc::clone(&record.core),
                    ));
                }
            }
            callbacks
        };
        for (extension, thread, _retained_core) in &callbacks {
            // SAFETY: the pending bit retains the live registry record and this
            // task-context pass invokes callbacks only after releasing its lock.
            unsafe {
                (extension.ops().on_deadline_overrun)(extension.data(), *thread);
            }
        }
        callbacks.len()
    }

    /// Creates a donation edge and a wake-before-block handshake token.
    pub fn pi_wait_start(
        &self,
        lock: PiLockId,
        waiter: ThreadId,
        owner: ThreadId,
    ) -> Result<PiWaitToken, TaskError> {
        let mut state = self.state.lock();
        if waiter == owner {
            return Err(TaskError::InvalidPiState);
        }
        if state.thread_record(waiter)?.lifecycle.state() == ThreadState::Exited
            || state.thread_record(owner)?.lifecycle.state() == ThreadState::Exited
        {
            return Err(TaskError::InvalidPiState);
        }
        match state.ensure_pi_acyclic(waiter, owner) {
            Ok(()) => {}
            Err(TaskError::PiCycle) => {
                drop(state);
                task_runtime::fatal_invariant(0x5049_0001, waiter.as_u64() as usize);
            }
            Err(error) => return Err(error),
        }
        state.thread_record(owner)?;
        let waiter_core = Arc::clone(&state.thread_record(waiter)?.core);
        if state.thread_record(waiter)?.blocked_on.is_some() {
            return Err(TaskError::InvalidPiState);
        }
        let next_waiter_count = state
            .thread_record(owner)?
            .blocked_pi_waiters
            .checked_add(1)
            .ok_or(TaskError::InvalidPiState)?;
        let generation = waiter_core.pi_wait_state().begin()?;
        state.thread_record_mut(waiter)?.blocked_on = Some(PiWaitRegistration {
            lock,
            owner,
            generation,
        });
        state.thread_record_mut(owner)?.blocked_pi_waiters = next_waiter_count;
        state.recompute_pi_chain(owner)?;
        Ok(PiWaitToken {
            core: waiter_core,
            generation,
        })
    }

    /// Cancels a waiter token after a wake-before-block handoff race.
    pub fn pi_wait_cancel(&self, token: PiWaitToken) -> Result<(), TaskError> {
        let mut state = self.state.lock();
        let waiter = token.waiter();
        let registration = state
            .thread_record(waiter)?
            .blocked_on
            .filter(|registration| registration.generation == token.generation)
            .ok_or(TaskError::InvalidPiState)?;
        state.thread_record_mut(waiter)?.blocked_on = None;
        let owner = state.thread_record_mut(registration.owner)?;
        owner.blocked_pi_waiters = owner
            .blocked_pi_waiters
            .checked_sub(1)
            .ok_or(TaskError::InvalidPiState)?;
        state.recompute_pi_chain(registration.owner)?;
        Ok(())
    }

    /// Transfers PI ownership and grants the selected wait token atomically.
    pub fn pi_mutex_handoff(
        &self,
        lock: PiLockId,
        old_owner: ThreadId,
        next_owner: Option<ThreadId>,
    ) -> Result<(), TaskError> {
        let mut state = self.state.lock();
        let active_waiters = state
            .slots
            .iter()
            .filter_map(|slot| slot.record.as_ref())
            .filter(|record| {
                record.blocked_on.is_some_and(|registration| {
                    registration.lock == lock && registration.owner == old_owner
                })
            })
            .count();
        let selected_waiter = next_owner.is_some_and(|next| {
            state.thread_record(next).is_ok_and(|record| {
                record.blocked_on.is_some_and(|registration| {
                    registration.lock == lock && registration.owner == old_owner
                })
            })
        });
        if (active_waiters == 0 && next_owner.is_some())
            || (active_waiters != 0 && !selected_waiter)
        {
            return Err(TaskError::InvalidPiState);
        }
        let redirected_waiters = active_waiters.saturating_sub(usize::from(selected_waiter));
        let next_waiter_count = next_owner
            .map(|next| {
                state
                    .thread_record(next)?
                    .blocked_pi_waiters
                    .checked_add(redirected_waiters)
                    .ok_or(TaskError::InvalidPiState)
            })
            .transpose()?;
        {
            let record = state.thread_record_mut(old_owner)?;
            if record.blocked_pi_waiters < active_waiters {
                return Err(TaskError::InvalidPiState);
            }
            record.blocked_pi_waiters -= active_waiters;
        }
        if let Some(next) = next_owner {
            for slot in &mut state.slots {
                let Some(record) = slot.record.as_mut() else {
                    continue;
                };
                let Some(registration) = record.blocked_on.as_mut() else {
                    continue;
                };
                if registration.lock != lock || registration.owner != old_owner {
                    continue;
                }
                if record.core.id() == next {
                    let generation = registration.generation;
                    record.blocked_on = None;
                    record.core.pi_wait_state().grant(generation)?;
                } else {
                    registration.owner = next;
                }
            }
            state.thread_record_mut(next)?.blocked_pi_waiters = next_waiter_count.unwrap_or(0);
        }
        state.recompute_pi_chain(old_owner)?;
        if let Some(next) = next_owner {
            state.recompute_pi_chain(next)?;
        }
        Ok(())
    }

    /// Captures stable state for deterministic scheduler comparisons.
    pub fn snapshot(&self, cpu: Pin<&CpuLocal>) -> CpuSnapshot {
        CpuSnapshot::capture(&cpu)
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

    fn activate_deadline_bandwidth(
        state: &mut TaskSystemState,
        mut cpu: Pin<&mut CpuLocal>,
        thread: ThreadId,
    ) -> Result<(), TaskError> {
        let owner = cpu.owner();
        let (is_deadline, assigned_cpu, activity, utilization_scaled) = {
            let record = state.thread_record(thread)?;
            (
                matches!(record.active_base_policy, SchedulePolicy::Deadline(_)),
                record.deadline_bandwidth_cpu,
                record.deadline_activity,
                record.deadline_bandwidth_scaled,
            )
        };
        if !is_deadline {
            return Ok(());
        }
        match assigned_cpu {
            None => cpu
                .as_mut()
                .fields_mut()
                .add_deadline_bandwidth(utilization_scaled, true)?,
            Some(assigned) if assigned != owner => {
                return Err(TaskError::CpuOwnerMismatch {
                    expected: assigned.as_u32(),
                    actual: owner.as_u32(),
                });
            }
            Some(_) if activity == DeadlineActivity::Inactive => cpu
                .as_mut()
                .fields_mut()
                .activate_deadline_bandwidth(utilization_scaled)?,
            Some(_) => {}
        }
        let record = state.thread_record_mut(thread)?;
        record.deadline_activity = DeadlineActivity::ActiveContending;
        record.deadline_bandwidth_cpu = Some(owner);
        record.deadline_zero_lag_ns = 0;
        Ok(())
    }

    fn detach_deadline_bandwidth(
        state: &mut TaskSystemState,
        mut cpu: Pin<&mut CpuLocal>,
        thread: ThreadId,
    ) -> Result<(), TaskError> {
        let owner = cpu.owner();
        let (assigned_cpu, activity, utilization_scaled) = {
            let record = state.thread_record(thread)?;
            (
                record.deadline_bandwidth_cpu,
                record.deadline_activity,
                record.deadline_bandwidth_scaled,
            )
        };
        let Some(assigned_cpu) = assigned_cpu else {
            return Ok(());
        };
        if assigned_cpu != owner {
            return Err(TaskError::CpuOwnerMismatch {
                expected: assigned_cpu.as_u32(),
                actual: owner.as_u32(),
            });
        }
        cpu.as_mut().fields_mut().remove_deadline_bandwidth(
            utilization_scaled,
            activity != DeadlineActivity::Inactive,
        )?;
        state.thread_record_mut(thread)?.deadline_bandwidth_cpu = None;
        Ok(())
    }

    fn assign_inactive_deadline_bandwidth(
        state: &mut TaskSystemState,
        mut cpu: Pin<&mut CpuLocal>,
        thread: ThreadId,
    ) -> Result<(), TaskError> {
        let owner = cpu.owner();
        let (is_deadline, assigned_cpu, utilization_scaled) = {
            let record = state.thread_record(thread)?;
            (
                matches!(record.active_base_policy, SchedulePolicy::Deadline(_)),
                record.deadline_bandwidth_cpu,
                record.deadline_bandwidth_scaled,
            )
        };
        if !is_deadline {
            return Ok(());
        }
        match assigned_cpu {
            None => cpu
                .as_mut()
                .fields_mut()
                .add_deadline_bandwidth(utilization_scaled, false)?,
            Some(assigned) if assigned != owner => {
                return Err(TaskError::CpuOwnerMismatch {
                    expected: assigned.as_u32(),
                    actual: owner.as_u32(),
                });
            }
            Some(_) => return Ok(()),
        }
        let record = state.thread_record_mut(thread)?;
        record.deadline_activity = DeadlineActivity::Inactive;
        record.deadline_bandwidth_cpu = Some(owner);
        record.deadline_zero_lag_ns = 0;
        Ok(())
    }

    fn mark_deadline_non_contending(
        state: &mut TaskSystemState,
        mut cpu: Pin<&mut CpuLocal>,
        thread: ThreadId,
        now_ns: u64,
    ) -> Result<(), TaskError> {
        let owner = cpu.owner();
        let (assigned_cpu, activity, utilization_scaled, deadline) = {
            let record = state.thread_record(thread)?;
            (
                record.deadline_bandwidth_cpu,
                record.deadline_activity,
                record.deadline_bandwidth_scaled,
                record.base_deadline,
            )
        };
        let (Some(assigned_cpu), Some(deadline)) = (assigned_cpu, deadline) else {
            return Ok(());
        };
        if assigned_cpu != owner || activity != DeadlineActivity::ActiveContending {
            return Ok(());
        }
        let zero_lag_ns = deadline_zero_lag_ns(deadline);
        let record = state.thread_record_mut(thread)?;
        if zero_lag_ns <= now_ns {
            cpu.as_mut()
                .fields_mut()
                .deactivate_deadline_bandwidth(utilization_scaled)?;
            record.deadline_activity = DeadlineActivity::Inactive;
            record.deadline_zero_lag_ns = 0;
        } else {
            record.deadline_activity = DeadlineActivity::ActiveNonContending;
            record.deadline_zero_lag_ns = zero_lag_ns;
            cpu.arm_deferred_scheduler_deadline(zero_lag_ns);
        }
        Ok(())
    }

    fn enqueue_with_reason(
        &self,
        state: &mut TaskSystemState,
        mut cpu: Pin<&mut CpuLocal>,
        thread: ThreadId,
        now_ns: u64,
        reason: EnqueueReason,
    ) -> Result<(), TaskError> {
        state.ensure_cpu_online(&cpu)?;
        let owner = cpu.owner();
        let (policy, queued_entity) = {
            let record = state.thread_record_mut(thread)?;
            if record.lifecycle.state() != ThreadState::Ready {
                return Err(TaskError::NotReady);
            }
            if !record.affinity.contains(owner) {
                return Err(TaskError::InvalidCpu(owner.as_u32()));
            }
            let mut queued_entity = record.entity;
            if matches!(reason, EnqueueReason::Wake)
                && matches!(record.policy, SchedulePolicy::Deadline(_))
            {
                queued_entity.activate_deadline(now_ns);
                record.entity = queued_entity;
                if record.deadline_donor.is_none()
                    && let SchedulingEntity::Deadline(deadline) = queued_entity
                {
                    record.base_deadline = Some(deadline);
                }
            }
            (record.policy, queued_entity)
        };
        Self::activate_deadline_bandwidth(state, cpu.as_mut(), thread)?;
        let fields = cpu.as_mut().fields_mut();
        let queued_entity =
            fields
                .run_queue
                .enqueue(thread, policy, queued_entity, now_ns, reason)?;
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
        let record = state.thread_record_mut(thread)?;
        record.entity = queued_entity;
        record
            .core
            .publish_effective_schedule(policy, queued_entity);
        record.queued_cpu = Some(owner);
        record.core.set_target_cpu(owner);
        if matches!(
            reason,
            EnqueueReason::Wake | EnqueueReason::Replenished | EnqueueReason::Migrated
        ) && preempts_current
        {
            fields.request_reschedule();
        }
        Self::publish_cpu_load_summary(state, cpu.as_mut());
        Ok(())
    }

    fn requeue_running(
        &self,
        state: &mut TaskSystemState,
        mut cpu: Pin<&mut CpuLocal>,
        thread: ThreadId,
        now_ns: u64,
        reason: EnqueueReason,
    ) -> Result<(), TaskError> {
        let record = state.thread_record_mut(thread)?;
        if record.entity.is_deadline_throttled() && !record.pi_critical_rescue {
            if let SchedulingEntity::Deadline(deadline) = record.entity {
                record.base_deadline = Some(deadline);
                record.deadline_replenish_pending = true;
                cpu.as_mut()
                    .arm_deferred_scheduler_deadline(deadline.next_scheduler_event_ns());
            }
            record.transition(ThreadState::Blocked)?;
            record.running_cpu = None;
            cpu.as_mut().set_current(None);
            return Ok(());
        }
        record.transition(ThreadState::Ready)?;
        record.running_cpu = None;
        cpu.as_mut().set_current(None);
        if cpu.idle() == Some(thread) {
            return Ok(());
        }
        self.enqueue_with_reason(state, cpu, thread, now_ns, reason)
    }

    fn transfer_balance_candidate(
        &self,
        state: &mut TaskSystemState,
        mut cpu: Pin<&mut CpuLocal>,
        target: CpuId,
        now_ns: u64,
        reason: BalanceReason,
    ) -> Result<Option<ThreadId>, TaskError> {
        state.ensure_cpu_online(&cpu)?;
        state
            .cpu_remote(target)
            .ok_or(TaskError::CpuOffline(target.as_u32()))?;
        let source = cpu.owner();
        if source == target {
            return Ok(None);
        }
        let candidate = Self::select_balance_candidate(
            state,
            cpu.as_ref().get_ref(),
            Some(target),
            now_ns,
            reason,
        );
        let Some(candidate) = candidate else {
            return Ok(None);
        };
        let thread = candidate.id;
        let queued = cpu
            .as_mut()
            .fields_mut()
            .run_queue
            .dequeue(thread)
            .ok_or(TaskError::NotReady)?;
        Self::detach_deadline_bandwidth(state, cpu.as_mut(), thread)?;
        let core = {
            let record = state.thread_record_mut(thread)?;
            if record.lifecycle.state() != ThreadState::Ready || record.queued_cpu != Some(source) {
                return Err(TaskError::InvalidConfiguration);
            }
            record.entity = queued.entity;
            record.queued_cpu = None;
            record.migration_target = Some(target);
            record.core.set_target_cpu(target);
            Arc::clone(&record.core)
        };
        if matches!(candidate.policy, SchedulePolicy::Fair { .. }) {
            cpu.defer_fair_balance(now_ns, self.config.balance_interval_ns());
        }
        Self::publish_cpu_load_summary(state, cpu.as_mut());
        state.publish_migration_to(&core, target, source, target)?;
        Ok(Some(thread))
    }

    fn select_balance_candidate(
        state: &TaskSystemState,
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
            let record = match state.thread_record(candidate.id) {
                Ok(record) => record,
                Err(_) => return false,
            };
            let allowed_target = target.map_or_else(
                || {
                    state.cpus.iter().enumerate().any(|(index, registration)| {
                        let target = CpuId::new(index as u32);
                        registration.online
                            && target != source
                            && record.affinity.contains(target)
                            && state
                                .cpu_remote(target)
                                .is_some_and(CpuRemote::is_scheduler_ready)
                    })
                },
                |target| {
                    record.affinity.contains(target)
                        && state
                            .cpu_remote(target)
                            .is_some_and(CpuRemote::is_scheduler_ready)
                },
            );
            if !allowed_target
                || record.queued_cpu != Some(source)
                || record.migration_target.is_some()
                || record.on_cpu.is_some()
                || record.core.sleep_timer_cpu().is_some()
                || (matches!(record.active_base_policy, SchedulePolicy::Deadline(_))
                    && !record.affinity.covers(&state.online))
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

    fn publish_cpu_load_summary(state: &TaskSystemState, mut cpu: Pin<&mut CpuLocal>) {
        let fields = cpu.as_mut().fields_mut();
        let current_key = fields
            .current_dispatch
            .as_ref()
            .map(CurrentDispatch::scheduling_key);
        let current_non_idle = fields.current.is_some() && fields.current != fields.idle;
        let candidate =
            Self::select_balance_candidate(state, fields, None, u64::MAX, BalanceReason::Summary);
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

    fn service_deadline_timers(
        &self,
        state: &mut TaskSystemState,
        mut cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<(), TaskError> {
        let slot_count = state.slots.len();
        if slot_count == 0 {
            return Ok(());
        }
        let owner = cpu.owner();
        let start = cpu.deadline_scan_cursor() % slot_count;
        let examined = slot_count.min(cpu.batch_limit());
        for offset in 0..examined {
            let index = (start + offset) % slot_count;
            let mut update_queued = None;
            let mut replenish = None;
            {
                let Some(record) = state.slots[index].record.as_mut() else {
                    continue;
                };
                if record.deadline_bandwidth_cpu == Some(owner)
                    && record.deadline_activity == DeadlineActivity::ActiveNonContending
                {
                    if now_ns >= record.deadline_zero_lag_ns {
                        cpu.as_mut()
                            .fields_mut()
                            .deactivate_deadline_bandwidth(record.deadline_bandwidth_scaled)?;
                        record.deadline_activity = DeadlineActivity::Inactive;
                        record.deadline_zero_lag_ns = 0;
                    } else {
                        cpu.arm_deferred_scheduler_deadline(record.deadline_zero_lag_ns);
                    }
                }
                let local_owner = record
                    .running_cpu
                    .or(record.queued_cpu)
                    .or(record.deadline_bandwidth_cpu)
                    .or_else(|| record.core.target_cpu());
                if local_owner != Some(owner) {
                    continue;
                }
                let Some(mut deadline) = record.base_deadline else {
                    continue;
                };
                let missed = deadline.observe_time(now_ns);
                let replenish_due =
                    deadline.is_throttled() && now_ns >= deadline.next_scheduler_event_ns();
                let next_event_ns = deadline.next_scheduler_event_ns();
                if !replenish_due && next_event_ns > now_ns {
                    // Zero-lag, CBS replenishment and deadline observation
                    // share one owner-CPU one-shot. Servicing an earlier event
                    // must explicitly retain the later event for blocked
                    // records that are absent from both current and runqueue.
                    cpu.arm_deferred_scheduler_deadline(next_event_ns);
                }
                if replenish_due {
                    deadline.replenish(now_ns);
                    record.base_deadline = Some(deadline);
                    if record.deadline_donor.is_none() {
                        record.entity = SchedulingEntity::Deadline(deadline);
                        record
                            .core
                            .publish_effective_schedule(record.policy, record.entity);
                    }
                    if deadline.is_throttled() {
                        // Saturating time arithmetic can make the next CBS
                        // deadline unrepresentable. Keep the job blocked unless
                        // replenishment actually restored both time and budget.
                        cpu.arm_deferred_scheduler_deadline(deadline.next_scheduler_event_ns());
                        continue;
                    }
                    if record.deadline_replenish_pending {
                        record.deadline_replenish_pending = false;
                        match record.lifecycle.state() {
                            ThreadState::Blocked => {
                                record.transition(ThreadState::Waking)?;
                                record.transition(ThreadState::Ready)?;
                            }
                            ThreadState::Waking => record.transition(ThreadState::Ready)?,
                            ThreadState::Ready => {}
                            _ => return Err(TaskError::InvalidConfiguration),
                        }
                        replenish = Some(record.core.id());
                    } else if record.deadline_donor.is_none()
                        && let Some(queued_cpu) = record.queued_cpu
                    {
                        update_queued = Some((
                            record.core.id(),
                            queued_cpu,
                            SchedulingEntity::Deadline(deadline),
                        ));
                    }
                } else if missed {
                    record.base_deadline = Some(deadline);
                    if record.deadline_donor.is_none() {
                        record.entity = SchedulingEntity::Deadline(deadline);
                    }
                    update_queued = record.queued_cpu.map(|queued_cpu| {
                        (
                            record.core.id(),
                            queued_cpu,
                            SchedulingEntity::Deadline(deadline),
                        )
                    });
                }
            }
            if let Some((thread, queued_cpu, entity)) = update_queued
                && queued_cpu == owner
                && !cpu
                    .as_mut()
                    .fields_mut()
                    .run_queue
                    .update_deadline_entity(thread, entity)
            {
                return Err(TaskError::InvalidConfiguration);
            }
            if let Some(thread) = replenish {
                self.enqueue_with_reason(
                    state,
                    cpu.as_mut(),
                    thread,
                    now_ns,
                    EnqueueReason::Replenished,
                )?;
            }
        }
        cpu.as_mut()
            .fields_mut()
            .set_deadline_scan_cursor((start + examined) % slot_count);
        if examined < slot_count {
            cpu.request_scheduler_work();
        }
        cpu.as_mut().refresh_scheduler_deadline(now_ns);
        Ok(())
    }

    fn program_local_timer(mut cpu: Pin<&mut CpuLocal>, now_ns: u64) -> Result<(), TaskError> {
        cpu.as_mut().refresh_scheduler_deadline(now_ns);
        let resolution_ns = task_runtime::timer_resolution_ns();
        let Some(deadline_ns) = cpu.as_ref().next_oneshot_deadline_ns(now_ns, resolution_ns) else {
            return Ok(());
        };
        ensure_runtime_success(task_runtime::program_oneshot_timer(deadline_ns))
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
        let mut state = self.state.lock();
        state.ensure_cpu_online(&cpu)?;
        Self::publish_cpu_load_summary(&state, cpu.as_mut());
        let source = cpu.owner();
        let source_load = cpu.runnable_summary();
        let target = state
            .cpus
            .iter()
            .enumerate()
            .filter(|(index, registration)| {
                registration.online && CpuId::new(*index as u32) != source
            })
            .filter_map(|(index, _)| {
                let target = CpuId::new(index as u32);
                let target_summary = state.cpu_remote(target)?.load_summary();
                if target_summary.runnable_count() >= source_load {
                    return None;
                }
                Self::select_balance_candidate(
                    &state,
                    cpu.as_ref().get_ref(),
                    Some(target),
                    now_ns,
                    BalanceReason::FairPeriodic,
                )?;
                Some((target_summary.runnable_count(), target))
            })
            .min_by_key(|(load, target)| (*load, target.as_u32()))
            .map(|(_, target)| target);
        let migrated = if let Some(target) = target {
            self.transfer_balance_candidate(
                &mut state,
                cpu.as_mut(),
                target,
                now_ns,
                BalanceReason::FairPeriodic,
            )?
        } else {
            None
        };
        cpu.defer_fair_balance(now_ns, self.config.balance_interval_ns());
        Ok(migrated)
    }

    fn migrate_running(
        &self,
        state: &mut TaskSystemState,
        mut cpu: Pin<&mut CpuLocal>,
        thread: ThreadId,
    ) -> Result<CpuId, TaskError> {
        let target = {
            let record = state.thread_record_mut(thread)?;
            let target = record
                .migration_target
                .ok_or(TaskError::InvalidConfiguration)?;
            if !record.affinity.contains(target) {
                return Err(TaskError::InvalidCpu(target.as_u32()));
            }
            record.transition(ThreadState::Ready)?;
            record.running_cpu = None;
            record.core.set_target_cpu(target);
            target
        };
        cpu.as_mut().set_current(None);
        Ok(target)
    }

    fn stage_switch_handoff(
        mut cpu: Pin<&mut CpuLocal>,
        previous: Option<ThreadId>,
        next: ThreadId,
        migration_target: Option<CpuId>,
    ) -> Result<(), TaskError> {
        match previous {
            Some(previous) if previous != next => cpu
                .as_mut()
                .stage_switch_handoff(previous, migration_target),
            _ if migration_target.is_none() => Ok(()),
            _ => Err(TaskError::InvalidConfiguration),
        }
    }

    fn pick_next(
        state: &mut TaskSystemState,
        mut cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
        outgoing: Option<ThreadId>,
    ) -> Result<ThreadId, TaskError> {
        let owner = cpu.owner();
        let fields = cpu.as_mut().fields_mut();
        let ordinary_rt_may_run = fields.rt_bandwidth.may_run(now_ns, false);
        if let Some(queued) = fields
            .run_queue
            .pick_next_with_rt(ordinary_rt_may_run, |thread| {
                state
                    .thread_record(thread)
                    .is_ok_and(ThreadRecord::is_pi_boosted_rt_owner)
            })
        {
            let record = state.thread_record_mut(queued.id)?;
            Self::validate_next_on_cpu(record, queued.id, owner, outgoing)?;
            record.entity = queued.entity;
            record.queued_cpu = None;
            record.running_cpu = Some(owner);
            record.on_cpu = Some(owner);
            record.transition(ThreadState::Running)?;
            fields.current = Some(queued.id);
            fields.remote().publish_current_thread(Some(queued.id));
            fields.remote().mark_scheduler_ready();
            fields.current_dispatch = Some(CurrentDispatch::new(
                CurrentDispatchState {
                    thread: queued.id,
                    policy: record.policy,
                    entity: record.entity,
                    deadline_donor: record.deadline_donor,
                    blocks_pi_waiter: record.blocked_pi_waiters != 0,
                    rt_quota_exempt: record.is_pi_boosted_rt_owner(),
                    pi_critical_rescue: record.pi_critical_rescue,
                    policy_generation: record.applied_policy_generation,
                },
                &record.core,
                now_ns,
            ));
            Self::publish_cpu_load_summary(state, cpu.as_mut());
            return Ok(queued.id);
        }
        let idle = fields.idle.ok_or(TaskError::NoRunnableThread)?;
        let record = state.thread_record_mut(idle)?;
        Self::validate_next_on_cpu(record, idle, owner, outgoing)?;
        if record.lifecycle.state() == ThreadState::Ready {
            record.transition(ThreadState::Running)?;
        }
        record.running_cpu = Some(owner);
        record.on_cpu = Some(owner);
        fields.current = Some(idle);
        fields.remote().publish_current_thread(Some(idle));
        fields.remote().mark_scheduler_ready();
        fields.current_dispatch = Some(CurrentDispatch::new(
            CurrentDispatchState {
                thread: idle,
                policy: record.policy,
                entity: record.entity,
                deadline_donor: record.deadline_donor,
                blocks_pi_waiter: record.blocked_pi_waiters != 0,
                rt_quota_exempt: record.is_pi_boosted_rt_owner(),
                pi_critical_rescue: record.pi_critical_rescue,
                policy_generation: record.applied_policy_generation,
            },
            &record.core,
            now_ns,
        ));
        Self::publish_cpu_load_summary(state, cpu.as_mut());
        Ok(idle)
    }

    fn validate_next_on_cpu(
        record: &ThreadRecord,
        next: ThreadId,
        owner: CpuId,
        outgoing: Option<ThreadId>,
    ) -> Result<(), TaskError> {
        match record.on_cpu {
            None => Ok(()),
            // The owner may select its currently executing thread again after
            // requeueing it for this same decision. No physical switch or
            // switch-tail ownership transfer occurs in that case.
            Some(executing_cpu) if outgoing == Some(next) && executing_cpu == owner => Ok(()),
            // All other non-empty states prove either a stale publication or a
            // second CPU trying to select a context whose switch tail has not
            // completed yet. Never overwrite the sole ownership authority.
            Some(_) => Err(TaskError::InvalidConfiguration),
        }
    }

    fn commit_current_dispatch(
        state: &mut TaskSystemState,
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
        if cpu.current() != Some(dispatch.thread) {
            return Err(TaskError::InvalidConfiguration);
        }
        dispatch.finish_runtime_accounting(now_ns);
        if state
            .thread_record(dispatch.thread)?
            .applied_policy_generation
            != dispatch.policy_generation
        {
            return Err(TaskError::InvalidConfiguration);
        }

        if let Some(donor) = dispatch.deadline_donor {
            let donor_record = state.thread_record_mut(donor)?;
            let SchedulingEntity::Deadline(deadline) = dispatch.entity else {
                return Err(TaskError::InvalidPiState);
            };
            donor_record.base_deadline = Some(deadline);
            if matches!(donor_record.active_base_policy, SchedulePolicy::Deadline(_)) {
                donor_record.entity = SchedulingEntity::Deadline(deadline);
            }
            donor_record.deadline_overrun_pending |= dispatch.deadline_overrun;
        }

        let record = state.thread_record_mut(dispatch.thread)?;
        record.charged_runtime_ns = record
            .charged_runtime_ns
            .saturating_add(dispatch.charged_runtime_ns());
        record.entity = dispatch.entity;
        record.pi_critical_rescue = dispatch.pi_critical_rescue;
        if dispatch.deadline_donor.is_none() {
            if let SchedulingEntity::Deadline(deadline) = dispatch.entity {
                record.base_deadline = Some(deadline);
            }
            record.deadline_overrun_pending |= dispatch.deadline_overrun;
        }
        Ok(())
    }
}

impl TaskSystemState {
    fn reserve_deadline(
        &mut self,
        policy: SchedulePolicy,
        affinity: &CpuSet,
    ) -> Result<u128, TaskError> {
        match policy {
            SchedulePolicy::Deadline(deadline) => {
                if !affinity.covers(&self.online) {
                    return Err(TaskError::DeadlineAffinity);
                }
                self.deadline_admission.reserve(deadline)
            }
            _ => Ok(0),
        }
    }

    fn deadline_reservation_for(
        &self,
        policy: SchedulePolicy,
        affinity: &CpuSet,
    ) -> Result<u128, TaskError> {
        match policy {
            SchedulePolicy::Deadline(deadline) => {
                if !affinity.covers(&self.online) {
                    return Err(TaskError::DeadlineAffinity);
                }
                Ok(DeadlineAdmission::utilization(deadline))
            }
            _ => Ok(0),
        }
    }

    fn allocate_thread_slot(&mut self) -> (u32, u32) {
        if let Some(slot) = self.free_slots.pop() {
            (slot, self.slots[slot as usize].generation)
        } else {
            let slot = self.slots.len() as u32;
            self.slots.push(ThreadSlot {
                generation: 1,
                record: None,
            });
            (slot, 1)
        }
    }

    fn thread_record(&self, thread: ThreadId) -> Result<&ThreadRecord, TaskError> {
        let slot = self
            .slots
            .get(thread.slot() as usize)
            .ok_or(TaskError::StaleThreadId)?;
        if slot.generation != thread.generation() {
            return Err(TaskError::StaleThreadId);
        }
        slot.record.as_ref().ok_or(TaskError::StaleThreadId)
    }

    fn thread_record_mut(&mut self, thread: ThreadId) -> Result<&mut ThreadRecord, TaskError> {
        let slot = self
            .slots
            .get_mut(thread.slot() as usize)
            .ok_or(TaskError::StaleThreadId)?;
        if slot.generation != thread.generation() {
            return Err(TaskError::StaleThreadId);
        }
        slot.record.as_mut().ok_or(TaskError::StaleThreadId)
    }

    fn cpu_registration(&self, cpu: CpuId) -> Result<&CpuRegistration, TaskError> {
        self.cpus
            .get(cpu.as_usize())
            .ok_or(TaskError::InvalidCpu(cpu.as_u32()))
    }

    fn cpu_registration_mut(&mut self, cpu: CpuId) -> Result<&mut CpuRegistration, TaskError> {
        self.cpus
            .get_mut(cpu.as_usize())
            .ok_or(TaskError::InvalidCpu(cpu.as_u32()))
    }

    fn ensure_cpu_online(&self, cpu: &CpuLocal) -> Result<(), TaskError> {
        let registration = self.cpu_registration(cpu.owner())?;
        if registration.online && cpu.is_online() {
            Ok(())
        } else {
            Err(TaskError::CpuOffline(cpu.owner().as_u32()))
        }
    }

    fn online_cpu_count(&self) -> usize {
        self.cpus.iter().filter(|cpu| cpu.online).count()
    }

    fn release_deadline_reservation_on_exit(&mut self, thread: ThreadId) -> Result<(), TaskError> {
        let held = {
            let record = self.thread_record_mut(thread)?;
            let held = record
                .active_deadline_reservation
                .max(record.desired_deadline_reservation);
            record.active_deadline_reservation = 0;
            record.desired_deadline_reservation = 0;
            held
        };
        self.deadline_admission.release(held);
        Ok(())
    }

    fn remove_exited_thread(&mut self, thread: ThreadId) -> Result<ThreadRecord, TaskError> {
        self.remove_exited_thread_with_count(thread, 1, None)
    }

    fn remove_exited_thread_with_count(
        &mut self,
        thread: ThreadId,
        expected_strong_count: usize,
        expected_core: Option<*const ThreadCore>,
    ) -> Result<ThreadRecord, TaskError> {
        let slot_index = thread.slot() as usize;
        let slot = self
            .slots
            .get_mut(slot_index)
            .ok_or(TaskError::StaleThreadId)?;
        if slot.generation != thread.generation() {
            return Err(TaskError::StaleThreadId);
        }
        let record = slot.record.as_ref().ok_or(TaskError::StaleThreadId)?;
        if !record.core.try_claim_reap() {
            return Err(TaskError::ThreadBusy);
        }
        let validation = (|| {
            if record.lifecycle.state() != ThreadState::Exited {
                return Err(TaskError::NotExited);
            }
            if record.on_cpu.is_some()
                || record.exit_callback_pending
                || record.exit_callback_claimed
            {
                return Err(TaskError::ThreadBusy);
            }
            if record.core.sleep_timer_cpu().is_some() {
                // The owner CPU's timer heap still contains a raw pointer to the
                // embedded node. Expiry/cancel must physically detach it before
                // this Arc allocation can be released.
                return Err(TaskError::ThreadBusy);
            }
            if expected_core.is_some_and(|core| !core::ptr::eq(core, Arc::as_ptr(&record.core))) {
                return Err(TaskError::StaleThreadId);
            }
            if Arc::strong_count(&record.core) != expected_strong_count {
                return Err(TaskError::ThreadBusy);
            }
            Ok(())
        })();
        if let Err(error) = validation {
            record.core.cancel_reap_claim();
            return Err(error);
        }
        let record = slot.record.take().ok_or(TaskError::StaleThreadId)?;
        self.deadline_admission.release(
            record
                .active_deadline_reservation
                .max(record.desired_deadline_reservation),
        );
        slot.generation = next_generation(slot.generation);
        self.free_slots.push(thread.slot());
        Ok(record)
    }

    fn remove_exited_thread_with_handle(
        &mut self,
        handle: &ThreadHandle,
    ) -> Result<ThreadRecord, TaskError> {
        self.remove_exited_thread_with_count(handle.id(), 2, Some(Arc::as_ptr(&handle.core)))
    }

    fn take_unreferenced_exited(&mut self) -> Result<Option<ThreadRecord>, TaskError> {
        for index in 0..self.slots.len() {
            let thread = {
                let slot = &self.slots[index];
                let Some(record) = slot.record.as_ref() else {
                    continue;
                };
                if record.lifecycle.state() != ThreadState::Exited
                    || record.on_cpu.is_some()
                    || record.exit_callback_pending
                    || record.exit_callback_claimed
                    || record.core.sleep_timer_cpu().is_some()
                {
                    continue;
                }
                let slot_index = u32::try_from(index)
                    .expect("thread registry slot must fit the ThreadId representation");
                ThreadId::from_parts(slot_index, slot.generation)
            };
            match self.remove_exited_thread_with_count(thread, 1, None) {
                Ok(record) => return Ok(Some(record)),
                Err(TaskError::ThreadBusy) => continue,
                Err(error) => return Err(error),
            }
        }
        Ok(None)
    }

    fn claim_pending_exit_callback(
        &mut self,
    ) -> Result<Option<(ThreadExtensionView, ThreadId)>, TaskError> {
        for (index, slot) in self.slots.iter_mut().enumerate() {
            let Some(record) = slot.record.as_mut() else {
                continue;
            };
            if record.lifecycle.state() != ThreadState::Exited
                || record.on_cpu.is_some()
                || !record.exit_callback_pending
                || record.exit_callback_claimed
            {
                continue;
            }
            let extension = record
                .extension
                .as_ref()
                .ok_or(TaskError::InvalidConfiguration)?
                .as_view();
            record.exit_callback_claimed = true;
            let slot_index = u32::try_from(index).map_err(|_| TaskError::InvalidConfiguration)?;
            return Ok(Some((
                extension,
                ThreadId::from_parts(slot_index, slot.generation),
            )));
        }
        Ok(None)
    }

    fn finish_exit_callback(&mut self, thread: ThreadId) -> Result<(), TaskError> {
        let record = self.thread_record_mut(thread)?;
        if record.lifecycle.state() != ThreadState::Exited
            || record.on_cpu.is_some()
            || !record.exit_callback_pending
            || !record.exit_callback_claimed
        {
            return Err(TaskError::InvalidConfiguration);
        }
        record.exit_callback_pending = false;
        record.exit_callback_claimed = false;
        Ok(())
    }

    fn ensure_pi_acyclic(&self, waiter: ThreadId, mut owner: ThreadId) -> Result<(), TaskError> {
        for _ in 0..self.slots.len().saturating_add(1) {
            if owner == waiter {
                return Err(TaskError::PiCycle);
            }
            let Some(registration) = self.thread_record(owner)?.blocked_on else {
                return Ok(());
            };
            owner = registration.owner;
        }
        Err(TaskError::PiCycle)
    }

    fn select_allowed_cpu(&self, affinity: &CpuSet) -> Option<CpuId> {
        self.cpus
            .iter()
            .enumerate()
            .filter(|(index, registration)| {
                registration.online && affinity.contains(CpuId::new(*index as u32))
            })
            .filter_map(|(index, registration)| {
                let cpu = CpuId::new(index as u32);
                registration
                    .remote
                    .is_online()
                    .then(|| (registration.remote.runnable_summary(), cpu))
            })
            .min_by_key(|(load, cpu)| (*load, cpu.as_u32()))
            .map(|(_, cpu)| cpu)
    }

    fn publish_migration_request(
        &self,
        core: &Arc<ThreadCore>,
        source: CpuId,
        target: CpuId,
    ) -> Result<(), TaskError> {
        self.publish_migration_to(core, source, source, target)
    }

    fn publish_migration_to(
        &self,
        core: &Arc<ThreadCore>,
        inbox_cpu: CpuId,
        source: CpuId,
        target: CpuId,
    ) -> Result<(), TaskError> {
        let cpu_local = self
            .cpu_remote(inbox_cpu)
            .ok_or(TaskError::CpuOffline(inbox_cpu.as_u32()))?;
        let pointer = Arc::as_ptr(core);
        // SAFETY: the retained count is transferred to the intrusive inbox
        // message and released by exactly one owner drain.
        unsafe { Arc::increment_strong_count(pointer) };
        // SAFETY: Arc allocation addresses are stable and the retained count
        // keeps the embedded migration node alive while queued.
        let node = unsafe { Pin::new_unchecked((*pointer).migration_node()) };
        let message = InboxMessage::migration_with_payload(
            core.id(),
            source,
            target,
            core.id().generation() as u64,
            pointer.expose_provenance(),
        );
        let result = cpu_local.publish_migration(node, message);
        if result != PublishResult::Published {
            // SAFETY: a rejected/coalesced publication did not consume this
            // attempt's retained reference.
            unsafe { Arc::decrement_strong_count(pointer) };
        }
        Ok(())
    }

    fn request_owner_reschedule(&self, owner: ThreadId) {
        if let Ok(record) = self.thread_record(owner)
            && let Some(cpu) = record
                .running_cpu
                .or(record.queued_cpu)
                .or(record.deadline_bandwidth_cpu)
        {
            let core = Arc::as_ptr(&record.core);
            let Some(cpu_local) = self.cpu_remote(cpu) else {
                return;
            };
            // SAFETY: this retained Arc count is transferred to the embedded
            // policy-update node and released by the owner drain.
            unsafe { Arc::increment_strong_count(core) };
            // SAFETY: the retained Arc count keeps this embedded node pinned.
            let node = unsafe { Pin::new_unchecked((*core).policy_update_node()) };
            let message = InboxMessage::migration_with_payload(
                owner,
                cpu,
                cpu,
                record.policy_generation,
                core.expose_provenance(),
            );
            let result = cpu_local.publish_policy_update(node, message);
            if result != PublishResult::Published {
                // SAFETY: rejected/coalesced publication did not consume the
                // retained count allocated for this attempt.
                unsafe { Arc::decrement_strong_count(core) };
            }
        }
    }

    fn apply_base_policy_generation(
        &mut self,
        thread: ThreadId,
        generation: u64,
        fair_slice_ns: u64,
        now_ns: u64,
        fair_placement: Option<FairPolicyPlacement>,
        activate_deadline: bool,
    ) -> Result<bool, TaskError> {
        let (
            latest_generation,
            applied_generation,
            policy,
            previous_entity,
            active_reservation,
            desired_reservation,
        ) = {
            let record = self.thread_record(thread)?;
            (
                record.policy_generation,
                record.applied_policy_generation,
                record.base_policy,
                record.entity,
                record.active_deadline_reservation,
                record.desired_deadline_reservation,
            )
        };
        if generation > latest_generation {
            return Ok(false);
        }
        if applied_generation == latest_generation {
            return Ok(false);
        }

        let mut entity = match (previous_entity, policy) {
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
                policy,
                fair_slice_ns,
                fair_placement.map_or(0, |placement| placement.destination_virtual_time),
            ),
        };
        if activate_deadline {
            entity.activate_deadline(now_ns);
        }
        let base_deadline = match entity {
            SchedulingEntity::Deadline(deadline) => Some(deadline),
            _ => None,
        };
        let record = self.thread_record_mut(thread)?;
        record.active_base_policy = policy;
        record.policy = policy;
        record.entity = entity;
        record.base_deadline = base_deadline;
        record.deadline_bandwidth_scaled = u64::try_from(desired_reservation).unwrap_or(u64::MAX);
        if record.deadline_bandwidth_cpu.is_none() {
            record.deadline_activity = DeadlineActivity::Inactive;
            record.deadline_zero_lag_ns = 0;
        }
        record.active_deadline_reservation = desired_reservation;
        record.applied_policy_generation = latest_generation;
        record.core.publish_effective_schedule(policy, entity);
        self.deadline_admission.release(
            active_reservation
                .max(desired_reservation)
                .saturating_sub(desired_reservation),
        );
        self.recompute_pi_chain(thread)?;
        Ok(true)
    }

    fn recompute_pi_chain(&mut self, start: ThreadId) -> Result<(), TaskError> {
        let mut current = start;
        for _ in 0..=self.slots.len() {
            let (base, base_entity, blocked_on, previous_policy, previous_donor) = {
                let record = self.thread_record(current)?;
                let base_entity = match record.active_base_policy {
                    SchedulePolicy::Deadline(_) => record
                        .base_deadline
                        .map(SchedulingEntity::Deadline)
                        .unwrap_or(record.entity),
                    _ if record.policy == record.active_base_policy => record.entity,
                    _ => SchedulingEntity::new(record.active_base_policy, self.fair_slice_ns, 0),
                };
                (
                    record.active_base_policy,
                    base_entity,
                    record.blocked_on,
                    record.policy,
                    record.deadline_donor,
                )
            };
            let mut effective = base;
            let mut effective_entity = base_entity;
            let mut effective_key = base_entity.scheduling_key(base, current.as_u64());
            let mut deadline_donor = None;
            for slot in &self.slots {
                let Some(donor_record) = slot.record.as_ref() else {
                    continue;
                };
                let Some(registration) = donor_record.blocked_on else {
                    continue;
                };
                if registration.owner != current {
                    continue;
                }
                let waiter = donor_record.core.id();
                let donor_policy = donor_record.policy;
                let donor = donor_record.deadline_donor.unwrap_or(waiter);
                let donor_entity = if matches!(donor_policy, SchedulePolicy::Deadline(_)) {
                    self.thread_record(donor)?
                        .base_deadline
                        .map(SchedulingEntity::Deadline)
                        .ok_or(TaskError::InvalidPiState)?
                } else {
                    donor_record.entity
                };
                let donor_key = donor_entity.scheduling_key(donor_policy, donor.as_u64());
                if donor_key < effective_key {
                    effective = donor_policy;
                    effective_entity = donor_entity;
                    effective_key = donor_key;
                    deadline_donor =
                        matches!(donor_policy, SchedulePolicy::Deadline(_)).then_some(donor);
                }
            }
            let changed = previous_policy != effective || previous_donor != deadline_donor;
            if changed {
                let record = self.thread_record_mut(current)?;
                record.policy = effective;
                record.deadline_donor = deadline_donor;
                record.entity = effective_entity;
            }
            let rescue_changed = {
                let record = self.thread_record(current)?;
                let should_rescue = record.blocked_pi_waiters != 0
                    && record
                        .entity
                        .deadline()
                        .is_some_and(|deadline| deadline.remaining_runtime_ns() == 0);
                should_rescue != record.pi_critical_rescue
            };
            if rescue_changed {
                let record = self.thread_record_mut(current)?;
                record.pi_critical_rescue = !record.pi_critical_rescue;
                if record.pi_critical_rescue {
                    record.entity.enter_pi_critical_rescue();
                } else {
                    record.entity.leave_pi_critical_rescue();
                }
                if record.deadline_donor.is_none()
                    && let SchedulingEntity::Deadline(deadline) = record.entity
                {
                    record.base_deadline = Some(deadline);
                }
            }
            if changed || rescue_changed {
                let record = self.thread_record(current)?;
                record
                    .core
                    .publish_effective_schedule(record.policy, record.entity);
                self.request_owner_reschedule(current);
            }
            let Some(registration) = blocked_on else {
                return Ok(());
            };
            current = registration.owner;
        }
        Err(TaskError::PiCycle)
    }

    fn refresh_effective_entity(
        &self,
        thread: ThreadId,
        fair_slice_ns: u64,
        now_ns: u64,
    ) -> Result<SchedulingEntity, TaskError> {
        let record = self.thread_record(thread)?;
        if let Some(donor) = record.deadline_donor {
            let mut entity = self
                .thread_record(donor)?
                .base_deadline
                .map(SchedulingEntity::Deadline)
                .ok_or(TaskError::InvalidPiState)?;
            if record.pi_critical_rescue {
                entity.enter_pi_critical_rescue();
            }
            return Ok(entity);
        }
        if record.policy == record.active_base_policy {
            if let Some(deadline) = record.base_deadline {
                return Ok(SchedulingEntity::Deadline(deadline));
            }
            if record.entity.matches_policy(record.policy) {
                return Ok(record.entity);
            }
        }
        Ok(SchedulingEntity::new(record.policy, fair_slice_ns, now_ns))
    }

    fn cpu_remote(&self, cpu: CpuId) -> Option<&CpuRemote> {
        let registration = self.cpu_registration(cpu).ok()?;
        if !registration.online || !registration.remote.is_online() {
            return None;
        }
        Some(registration.remote.as_ref())
    }

    fn switch_plan(
        &self,
        previous: Option<ThreadId>,
        next: ThreadId,
        switch_reason: SwitchReason,
    ) -> ScheduleDecision {
        let previous_endpoint = previous
            .and_then(|thread| self.thread_record(thread).ok())
            .map(SwitchEndpoint::from_record);
        let next_endpoint = self
            .thread_record(next)
            .map(SwitchEndpoint::from_record)
            .unwrap_or_else(|_| SwitchEndpoint::empty(next));
        ScheduleDecision {
            previous,
            next,
            previous_endpoint,
            next_endpoint,
            switch_reason,
        }
    }
}

/// Result of one scheduler safe-point decision.
#[derive(Clone, Copy, Debug)]
pub struct ScheduleDecision {
    previous: Option<ThreadId>,
    next: ThreadId,
    previous_endpoint: Option<SwitchEndpoint>,
    next_endpoint: SwitchEndpoint,
    switch_reason: SwitchReason,
}

/// Result of one bounded scheduler safe point.
///
/// This type deliberately keeps lifecycle deferral and inbox backpressure
/// separate from a scheduling decision. Callers must not infer either state
/// from a boolean `need_resched` value or an absent decision.
#[derive(Clone, Copy, Debug)]
pub enum SchedulerOutcome {
    /// No context switch or owner-only work remains from this pass.
    Quiescent,
    /// The current thread owns an in-flight park token and must finish it.
    ParkingDeferred,
    /// One bounded inbox batch completed, with more owner-only work retained.
    OwnerWorkPending,
    /// The scheduler selected a next thread.
    Decision(ScheduleDecision),
}

impl SchedulerOutcome {
    /// Returns the scheduler decision, if this pass selected a thread.
    pub const fn decision(self) -> Option<ScheduleDecision> {
        match self {
            Self::Decision(decision) => Some(decision),
            Self::Quiescent | Self::ParkingDeferred | Self::OwnerWorkPending => None,
        }
    }

    /// Returns whether the caller must finish a pending park handshake before
    /// scheduler task-work callbacks may execute.
    pub const fn parking_deferred(self) -> bool {
        matches!(self, Self::ParkingDeferred)
    }

    /// Returns whether more owner-only inbox work remains for a later bounded
    /// safe point.
    pub const fn owner_work_pending(self) -> bool {
        matches!(self, Self::OwnerWorkPending)
    }
}

impl ScheduleDecision {
    /// Returns the thread that stopped running, if any.
    pub const fn previous(self) -> Option<ThreadId> {
        self.previous
    }

    /// Returns the selected thread or CPU idle thread.
    pub const fn next(self) -> ThreadId {
        self.next
    }

    /// Returns why the previous thread relinquished the CPU.
    pub const fn switch_reason(self) -> SwitchReason {
        self.switch_reason
    }

    /// Returns whether the architecture execution context must change.
    pub fn requires_context_switch(self) -> bool {
        self.previous != Some(self.next)
    }

    pub(crate) const fn previous_endpoint(self) -> Option<SwitchEndpoint> {
        self.previous_endpoint
    }

    pub(crate) const fn next_endpoint(self) -> SwitchEndpoint {
        self.next_endpoint
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SwitchEndpoint {
    thread: ThreadId,
    context: ExecutionContextHandle,
    address_space: crate::runtime::AddressSpaceHandle,
    extension: Option<ThreadExtensionView>,
}

impl SwitchEndpoint {
    fn from_record(record: &ThreadRecord) -> Self {
        Self {
            thread: record.core.id(),
            context: record.resources.context(),
            address_space: record.resources.address_space(),
            extension: record.extension.as_ref().map(ThreadExtension::as_view),
        }
    }

    const fn empty(thread: ThreadId) -> Self {
        Self {
            thread,
            context: ExecutionContextHandle::NONE,
            address_space: crate::runtime::AddressSpaceHandle::NONE,
            extension: None,
        }
    }

    pub(crate) const fn thread(self) -> ThreadId {
        self.thread
    }

    pub(crate) const fn context(self) -> ExecutionContextHandle {
        self.context
    }

    pub(crate) const fn address_space(self) -> crate::runtime::AddressSpaceHandle {
        self.address_space
    }

    pub(crate) const fn extension(self) -> Option<ThreadExtensionView> {
        self.extension
    }
}

/// Result of charging one scheduler dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChargeOutcome {
    slice_expired: bool,
    deadline_overrun: bool,
}

/// Snapshot of one Deadline reservation's CBS and PI state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeadlineRuntimeSnapshot {
    remaining_runtime_ns: u64,
    misses: u64,
    overruns: u64,
    pi_critical_rescue: bool,
    donor: Option<ThreadId>,
}

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

/// Snapshot of a Deadline thread's GRUB ownership and zero-lag state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeadlineActivitySnapshot {
    activity: DeadlineActivity,
    bandwidth_cpu: Option<CpuId>,
    zero_lag_ns: u64,
}

impl DeadlineActivitySnapshot {
    /// Returns the GRUB state.
    pub const fn activity(self) -> DeadlineActivity {
        self.activity
    }

    /// Returns the runqueue owning this reservation's `this_bw` contribution.
    pub const fn bandwidth_cpu(self) -> Option<CpuId> {
        self.bandwidth_cpu
    }

    /// Returns the pending zero-lag boundary, or zero when no timer is armed.
    pub const fn zero_lag_ns(self) -> u64 {
        self.zero_lag_ns
    }
}

impl DeadlineRuntimeSnapshot {
    /// Returns the remaining CBS runtime.
    pub const fn remaining_runtime_ns(self) -> u64 {
        self.remaining_runtime_ns
    }

    /// Returns observed absolute-deadline misses.
    pub const fn misses(self) -> u64 {
        self.misses
    }

    /// Returns observed CBS overruns.
    pub const fn overruns(self) -> u64 {
        self.overruns
    }

    /// Reports whether execution is in the explicit PI-critical rescue path.
    pub const fn pi_critical_rescue(self) -> bool {
        self.pi_critical_rescue
    }

    /// Returns the original Deadline reservation currently donated to the thread.
    pub const fn donor(self) -> Option<ThreadId> {
        self.donor
    }
}

/// Result of one bounded owner-side remote-wake drain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RemoteWakeDrain {
    drained: usize,
    pending: bool,
}

impl RemoteWakeDrain {
    /// Returns the number of detached wake messages consumed.
    pub const fn drained(self) -> usize {
        self.drained
    }

    /// Returns whether another bounded drain is required.
    pub const fn pending(self) -> bool {
        self.pending
    }
}

impl ChargeOutcome {
    /// Returns whether RR, fair service, or CBS budget reached its boundary.
    pub const fn slice_expired(self) -> bool {
        self.slice_expired
    }

    /// Returns whether CBS exhaustion entered a PI-critical rescue section.
    pub const fn deadline_overrun(self) -> bool {
        self.deadline_overrun
    }
}

#[derive(Debug)]
struct CpuRegistration {
    online: bool,
    remote: Arc<CpuRemote>,
}

#[derive(Debug)]
struct ThreadSlot {
    generation: u32,
    record: Option<ThreadRecord>,
}

#[derive(Debug)]
struct ThreadRecord {
    core: Arc<ThreadCore>,
    lifecycle: ThreadLifecycle,
    base_policy: SchedulePolicy,
    active_base_policy: SchedulePolicy,
    policy: SchedulePolicy,
    policy_generation: u64,
    applied_policy_generation: u64,
    affinity: CpuSet,
    extension: Option<ThreadExtension>,
    resources: ThreadResources,
    entity: SchedulingEntity,
    base_deadline: Option<DeadlineEntity>,
    deadline_activity: DeadlineActivity,
    deadline_bandwidth_cpu: Option<CpuId>,
    deadline_bandwidth_scaled: u64,
    deadline_zero_lag_ns: u64,
    active_deadline_reservation: u128,
    desired_deadline_reservation: u128,
    queued_cpu: Option<CpuId>,
    running_cpu: Option<CpuId>,
    on_cpu: Option<CpuId>,
    migration_target: Option<CpuId>,
    blocked_on: Option<PiWaitRegistration>,
    blocked_pi_waiters: usize,
    deadline_donor: Option<ThreadId>,
    pi_critical_rescue: bool,
    deadline_replenish_pending: bool,
    deadline_overrun_pending: bool,
    exit_callback_pending: bool,
    exit_callback_claimed: bool,
    charged_runtime_ns: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PiWaitRegistration {
    lock: PiLockId,
    owner: ThreadId,
    generation: u64,
}

impl ThreadRecord {
    fn transition(&mut self, state: ThreadState) -> Result<(), TaskError> {
        self.lifecycle.transition(state)?;
        self.core.publish_state(state);
        Ok(())
    }

    fn is_pi_boosted_rt_owner(&self) -> bool {
        self.blocked_pi_waiters != 0
            && self.policy != self.active_base_policy
            && matches!(
                self.policy,
                SchedulePolicy::Fifo { .. } | SchedulePolicy::RoundRobin { .. }
            )
    }

    fn has_live_pi_edges(&self) -> bool {
        self.blocked_on.is_some() || self.blocked_pi_waiters != 0
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

fn release_thread_record(mut record: ThreadRecord) -> Result<(), TaskError> {
    drop(record.extension.take());
    let resources = core::mem::replace(&mut record.resources, ThreadResources::NONE);
    resources.release()
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
    let next = generation.wrapping_add(1);
    if next == 0 { 1 } else { next }
}

#[cfg(test)]
mod tests {
    use alloc::{boxed::Box, vec::Vec};
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn publish_test_scheduler_work(
        remote: &CpuRemote,
        node: Pin<&'static crate::inbox::InboxNode>,
        slot: u32,
    ) {
        let message = InboxMessage::remote_wake(ThreadId::from_parts(slot, 1), remote.owner());
        let result = remote.publish_remote_wake(node, message);
        assert_eq!(result, PublishResult::Published);
    }

    fn test_inbox_node(
        node: &Pin<Box<crate::inbox::InboxNode>>,
    ) -> Pin<&'static crate::inbox::InboxNode> {
        let node = node.as_ref().get_ref() as *const crate::inbox::InboxNode;
        unsafe {
            // Callers keep the pinned fixture alive until its inbox has drained
            // or the complete owning task system has been dropped.
            Pin::new_unchecked(&*node)
        }
    }

    #[test]
    fn busy_scheduler_ipi_is_persistently_retried_without_a_new_producer() {
        let node = Box::pin(crate::inbox::InboxNode::new(InboxKind::RemoteWake));
        let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
        let remote = &system.cpu_remotes[1];
        remote.mark_online();
        crate::test_runtime::configure_scheduler_ipi(RuntimeStatus::Success, 1);

        publish_test_scheduler_work(remote, test_inbox_node(&node), 1);
        assert_eq!(crate::test_runtime::scheduler_ipi_send_count(), 1);
        assert!(system.scheduler_ipi_retry_pending());
        assert_eq!(remote.scheduler_ipi_fault_count(), 1);

        assert_eq!(system.service_scheduler_ipi_retries(64), Ok(1));
        assert_eq!(crate::test_runtime::scheduler_ipi_send_count(), 2);
        assert!(!system.scheduler_ipi_retry_pending());
    }

    #[test]
    fn permanent_scheduler_ipi_failure_is_quarantined_and_not_silent() {
        let node = Box::pin(crate::inbox::InboxNode::new(InboxKind::RemoteWake));
        let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
        let remote = &system.cpu_remotes[1];
        remote.mark_online();
        crate::test_runtime::configure_scheduler_ipi(RuntimeStatus::InvalidArgument, 0);

        publish_test_scheduler_work(remote, test_inbox_node(&node), 2);
        let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = system.service_scheduler_ipi_retries(64);
        }));
        assert!(
            failure.is_err(),
            "permanent transport failure must fail-stop"
        );
        assert!(!system.scheduler_ipi_retry_pending());
    }

    #[test]
    fn registered_remote_endpoint_is_separate_from_owner_mutable_state() {
        let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let owner_address = (cpu.as_ref().get_ref() as *const CpuLocal).addr();
        let endpoint_address = Arc::as_ptr(cpu.as_ref().get_ref().remote()).addr();
        assert_ne!(owner_address, endpoint_address);

        system.bring_cpu_online(cpu.as_mut()).unwrap();
        assert_eq!(
            (system.state.lock().cpu_remote(CpuId::new(0)).unwrap() as *const CpuRemote).addr(),
            endpoint_address
        );

        cpu.as_mut().set_current(None);
        assert_eq!(
            (system.state.lock().cpu_remote(CpuId::new(0)).unwrap() as *const CpuRemote).addr(),
            endpoint_address,
            "owner reborrowing must not alias or invalidate the remote endpoint"
        );
    }

    #[test]
    fn configuration_rejects_batch_larger_than_irq_contract() {
        assert!(matches!(
            TaskSystem::new(
                TaskSystemConfig::new(1).with_batch_limit(crate::DEFAULT_BATCH_LIMIT + 1)
            ),
            Err(TaskError::InvalidConfiguration)
        ));
    }
    use crate::{DeadlineFlags, DeadlinePolicy, FairMode, Nice, RtPriority, ThreadExtensionOps};

    static DEADLINE_OVERRUN_CALLBACKS: AtomicUsize = AtomicUsize::new(0);

    struct InstalledTaskHandles;

    impl InstalledTaskHandles {
        fn new(system: Pin<&TaskSystem>, cpu: Pin<&mut CpuLocal>) -> Self {
            crate::test_runtime::install_task_handles(
                (system.get_ref() as *const TaskSystem).expose_provenance(),
                // SAFETY: the test fixture keeps the owner object pinned and
                // serializes every scheduler access until the handle is cleared.
                (unsafe { Pin::get_unchecked_mut(cpu) } as *mut CpuLocal).expose_provenance(),
            );
            Self
        }
    }

    impl Drop for InstalledTaskHandles {
        fn drop(&mut self) {
            crate::test_runtime::clear_task_handles();
        }
    }

    #[test]
    fn generation_rejects_a_stale_registry_identity() {
        let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
        let first = system
            .create_thread(ThreadSpec::new(Default::default()))
            .unwrap();
        let first_id = first.id();
        system.mark_exited(first_id).unwrap();
        drop(first);
        system.reap_thread(first_id).unwrap();
        let second = system
            .create_thread(ThreadSpec::new(Default::default()))
            .unwrap();
        assert_eq!(first_id.slot(), second.id().slot());
        assert_eq!(system.thread_state(first_id), Err(TaskError::StaleThreadId));
    }

    #[test]
    fn detached_reaper_waits_for_the_last_external_handle() {
        let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
        let thread = system
            .create_thread(ThreadSpec::new(Default::default()))
            .unwrap();
        let id = thread.id();
        system.mark_exited(id).unwrap();

        assert_eq!(system.reap_unreferenced_exited(1).unwrap(), 0);
        drop(thread);
        assert_eq!(system.reap_unreferenced_exited(1).unwrap(), 1);
        assert_eq!(system.thread_state(id), Err(TaskError::StaleThreadId));
    }

    #[test]
    fn owned_reap_returns_handle_until_other_wake_references_are_gone() {
        let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
        let thread = system
            .create_thread(ThreadSpec::new(Default::default()))
            .unwrap();
        let id = thread.id();
        let late_wake = thread.wake_handle();
        system.mark_exited(id).unwrap();

        let error = system.reap_thread_handle(thread).unwrap_err();
        assert_eq!(error.task_error(), TaskError::ThreadBusy);
        let thread = error
            .into_retry_handle()
            .expect("busy owned reap must retain its generation-pinning handle");
        assert_eq!(system.reap_unreferenced_exited(1).unwrap(), 0);

        drop(late_wake);
        system.reap_thread_handle(thread).unwrap();
        assert_eq!(system.thread_state(id), Err(TaskError::StaleThreadId));
    }

    #[test]
    fn current_entry_can_release_its_lookup_lease_before_nonreturning_exit() {
        let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
        // SAFETY: the no-op callback table accepts the zero-sized test payload.
        let extension = unsafe { ThreadExtension::new(0, &DEADLINE_TEST_EXTENSION_OPS) };
        let thread = system
            .create_thread(ThreadSpec::new(Default::default()).with_extension(extension))
            .unwrap();
        let lease = system
            .thread_extension_lease(thread.clone())
            .unwrap()
            .unwrap();

        let view = unsafe {
            // SAFETY: the registry record retains the extension until this
            // test marks and reaps the current-entry model below.
            lease.release_for_current_thread_entry()
        };
        assert!(core::ptr::eq(view.ops(), &DEADLINE_TEST_EXTENSION_OPS));
        system.mark_exited(thread.id()).unwrap();
        system.reap_thread_handle(thread).unwrap();
    }

    #[test]
    fn reaper_waits_until_embedded_sleep_timer_is_detached() {
        let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
        let thread = system
            .create_thread(ThreadSpec::new(Default::default()))
            .unwrap();
        let id = thread.id();
        thread.core.register_sleep_timer(CpuId::new(0), 1);
        system.mark_exited(id).unwrap();

        assert_eq!(system.reap_thread(id), Err(TaskError::ThreadBusy));
        assert!(thread.core.complete_sleep_timer(1));
        system.reap_thread_handle(thread).unwrap();
    }

    #[test]
    fn exited_context_cannot_be_reaped_before_switch_tail() {
        let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let bootstrap = system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system
            .register_idle_thread(
                cpu.as_mut(),
                ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
            )
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();

        let exiting = bootstrap.id();
        drop(bootstrap);
        let decision = system.exit_current(cpu.as_mut()).unwrap();
        assert_ne!(decision.next(), exiting);
        assert_eq!(system.reap_thread(exiting), Err(TaskError::ThreadBusy));

        system.complete_context_switch(cpu.as_mut()).unwrap();
        system.reap_thread(exiting).unwrap();
    }

    #[test]
    fn switch_tail_defers_exit_callback_until_scheduler_guards_are_released() {
        EXIT_CALLBACK_INVOCATIONS.store(0, Ordering::Release);
        let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        // SAFETY: the test callback table owns no external resource and treats
        // the zero payload as an opaque value.
        let extension = unsafe { ThreadExtension::new(0, &EXIT_CALLBACK_TEST_OPS) };
        let bootstrap = system
            .install_bootstrap_thread(
                cpu.as_mut(),
                ThreadSpec::new(SchedulePolicy::default()).with_extension(extension),
            )
            .unwrap();
        system
            .register_idle_thread(
                cpu.as_mut(),
                ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
            )
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();

        let exiting = bootstrap.id();
        drop(bootstrap);
        system.exit_current(cpu.as_mut()).unwrap();
        system.complete_context_switch(cpu.as_mut()).unwrap();

        assert_eq!(
            EXIT_CALLBACK_INVOCATIONS.load(Ordering::Acquire),
            0,
            "context-switch tail must not invoke task-context exit callbacks"
        );
        assert_eq!(system.dispatch_exit_callbacks(1).unwrap(), 1);
        assert_eq!(EXIT_CALLBACK_INVOCATIONS.load(Ordering::Acquire), 1);
        assert_eq!(system.thread_state(exiting), Ok(ThreadState::Exited));
    }

    #[test]
    fn scheduler_work_without_preemption_preserves_current_dispatch() {
        let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();

        cpu.request_scheduler_work();
        assert!(matches!(
            system.schedule_if_requested(cpu.as_mut(), 1).unwrap(),
            SchedulerOutcome::Quiescent
        ));
        system
            .charge_current(cpu.as_mut(), 2, 1, 0)
            .expect("scheduler-only work must not discard the running dispatch");
    }

    #[test]
    fn fair_policy_update_reweights_lag_without_resetting_service_history() {
        let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let first = system
            .create_thread(ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        let second = system
            .create_thread(ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        for thread in [&first, &second] {
            system.make_ready(thread.id()).unwrap();
            system.enqueue(cpu.as_mut(), thread.id(), 0).unwrap();
        }

        assert_eq!(system.schedule(cpu.as_mut(), 0).unwrap().next(), first.id());
        system
            .charge_current(cpu.as_mut(), 400_000, 400_000, 0)
            .unwrap();
        assert_eq!(
            system.yield_current(cpu.as_mut(), 400_000).unwrap().next(),
            second.id()
        );
        system
            .charge_current(cpu.as_mut(), 800_000, 400_000, 0)
            .unwrap();
        assert_eq!(
            system.yield_current(cpu.as_mut(), 800_000).unwrap().next(),
            first.id()
        );
        system
            .charge_current(cpu.as_mut(), 1_050_000, 250_000, 0)
            .unwrap();

        let before = cpu
            .current_dispatch
            .as_ref()
            .unwrap()
            .entity
            .fair()
            .unwrap();
        assert_eq!(before.vruntime(), 650_000);
        assert_eq!(before.remaining_request_ns(), 350_000);
        let virtual_time = cpu.run_queue.virtual_time();
        assert_eq!(virtual_time, 825_000);

        let nice = Nice::new(5).unwrap();
        system
            .set_thread_policy(first.id(), SchedulePolicy::fair(nice, FairMode::Normal))
            .unwrap();
        system
            .drain_policy_updates(cpu.as_mut(), 1_050_000)
            .unwrap();
        let reweighted = system
            .state
            .lock()
            .thread_record(first.id())
            .unwrap()
            .entity
            .fair()
            .unwrap();
        let lag = (virtual_time as i128 - 650_000_i128) * Nice::ZERO.weight() as i128
            / nice.weight() as i128;
        let expected_vruntime = (virtual_time as i128 - lag) as u64;
        let expected_remaining_delta = (350_000_u128 * 1024 / nice.weight() as u128) as u64;
        assert_eq!(reweighted.vruntime(), expected_vruntime);
        assert_eq!(reweighted.remaining_request_ns(), 350_000);
        assert_eq!(
            reweighted.virtual_deadline(),
            expected_vruntime + expected_remaining_delta
        );

        system
            .set_thread_policy(first.id(), SchedulePolicy::fair(nice, FairMode::Batch))
            .unwrap();
        system
            .drain_policy_updates(cpu.as_mut(), 1_050_000)
            .unwrap();
        let batch = system
            .state
            .lock()
            .thread_record(first.id())
            .unwrap()
            .entity
            .fair()
            .unwrap();
        assert_eq!(batch.vruntime(), reweighted.vruntime());
        assert_eq!(batch.virtual_deadline(), reweighted.virtual_deadline());
        assert_eq!(batch.remaining_request_ns(), 350_000);

        system
            .set_thread_policy(
                first.id(),
                SchedulePolicy::fair(Nice::new(-20).unwrap(), FairMode::Idle),
            )
            .unwrap();
        system
            .drain_policy_updates(cpu.as_mut(), 1_050_000)
            .unwrap();
        let idle = system
            .state
            .lock()
            .thread_record(first.id())
            .unwrap()
            .entity
            .fair()
            .unwrap();
        assert_eq!(idle.nice(), Nice::LOWEST);
        assert_eq!(idle.remaining_request_ns(), 350_000);
    }

    #[test]
    fn running_idle_to_normal_transition_uses_both_class_virtual_times() {
        let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let idle = system
            .install_bootstrap_thread(
                cpu.as_mut(),
                ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
            )
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();

        let normal = system
            .create_thread(ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.make_ready(normal.id()).unwrap();
        system.enqueue(cpu.as_mut(), normal.id(), 0).unwrap();
        assert_eq!(
            system.schedule(cpu.as_mut(), 0).unwrap().next(),
            normal.id()
        );
        system
            .charge_current(cpu.as_mut(), 1_000_000, 1_000_000, 0)
            .unwrap();
        assert_eq!(
            system.block_current(cpu.as_mut()).unwrap().next(),
            idle.id()
        );
        system
            .charge_current(cpu.as_mut(), 1_001_000, 1_000, 0)
            .unwrap();

        let normal_virtual_time = cpu.run_queue.virtual_time();
        assert_eq!(normal_virtual_time, 1_000_000);
        system
            .set_thread_policy(idle.id(), SchedulePolicy::default())
            .unwrap();
        system
            .drain_policy_updates(cpu.as_mut(), 1_001_000)
            .unwrap();

        let transitioned = system
            .state
            .lock()
            .thread_record(idle.id())
            .unwrap()
            .entity
            .fair()
            .unwrap();
        assert_eq!(
            transitioned.vruntime(),
            normal_virtual_time,
            "a zero-lag entity must be rebased onto the destination class's V",
        );
    }

    #[test]
    fn running_normal_to_idle_transition_settles_then_rebases_lag() {
        let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let normal = system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();

        system
            .set_thread_policy(
                normal.id(),
                SchedulePolicy::fair(Nice::ZERO, FairMode::Idle),
            )
            .unwrap();
        system
            .drain_policy_updates(cpu.as_mut(), 1_000_000)
            .unwrap();

        let state = system.state.lock();
        let record = state.thread_record(normal.id()).unwrap();
        let transitioned = record.entity.fair().unwrap();
        assert_eq!(record.charged_runtime_ns, 1_000_000);
        assert_eq!(transitioned.mode(), FairMode::Idle);
        assert_eq!(
            transitioned.vruntime(),
            cpu.run_queue.virtual_time_for_mode(FairMode::Idle),
            "settled zero lag must be expressed relative to the destination V domain",
        );
    }

    #[test]
    fn bounded_inbox_remainder_stays_sticky_across_scheduler_entry() {
        let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();

        let mut nodes = Vec::with_capacity(cpu.batch_limit() * 2 + 1);
        for slot in 0..=cpu.batch_limit() * 2 {
            nodes.push(Box::pin(crate::inbox::InboxNode::new(
                crate::inbox::InboxKind::RemoteWake,
            )));
            let message =
                InboxMessage::remote_wake(ThreadId::from_parts(slot as u32, 1), CpuId::new(0));
            assert_eq!(
                cpu.remote()
                    .publish_remote_wake(test_inbox_node(nodes.last().unwrap()), message),
                PublishResult::Published
            );
        }

        let first = system.drain_remote_wakes(cpu.as_mut(), 1).unwrap();
        assert_eq!(first.drained(), cpu.batch_limit());
        assert!(first.pending());
        assert!(
            system
                .schedule_if_requested(cpu.as_mut(), 1)
                .unwrap()
                .owner_work_pending()
        );
        assert!(cpu.needs_reschedule());

        let second = system.drain_remote_wakes(cpu.as_mut(), 2).unwrap();
        assert_eq!(second.drained(), 1);
        assert!(!second.pending());
        assert!(matches!(
            system.schedule_if_requested(cpu.as_mut(), 2).unwrap(),
            SchedulerOutcome::Quiescent
        ));
        system.charge_current(cpu.as_mut(), 3, 1, 0).unwrap();
    }

    #[test]
    fn running_migration_is_published_only_after_switch_tail() {
        let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
        let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
        let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
        for cpu in [&mut cpu0, &mut cpu1] {
            system
                .register_idle_thread(
                    cpu.as_mut(),
                    ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
                )
                .unwrap();
            system.bring_cpu_online(cpu.as_mut()).unwrap();
        }
        let thread = system
            .create_thread(ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.make_ready(thread.id()).unwrap();
        system.enqueue(cpu0.as_mut(), thread.id(), 0).unwrap();
        assert_eq!(
            system.schedule(cpu0.as_mut(), 0).unwrap().next(),
            thread.id()
        );

        let mut target_only = CpuSet::empty(2);
        target_only.insert(CpuId::new(1));
        system.set_affinity(thread.id(), target_only).unwrap();
        system.drain_policy_updates(cpu0.as_mut(), 1).unwrap();
        let decision = system
            .schedule_if_requested(cpu0.as_mut(), 1)
            .unwrap()
            .decision()
            .unwrap();
        assert_eq!(decision.previous(), Some(thread.id()));
        assert!(!cpu1.has_remote_work());

        system.complete_context_switch(cpu0.as_mut()).unwrap();
        assert!(cpu1.has_remote_work());
        let transfer = system.drain_policy_updates(cpu1.as_mut(), 2).unwrap();
        assert_eq!(transfer.drained(), 1);
        assert!(!transfer.pending());
    }

    #[test]
    fn selection_rejects_a_thread_still_executing_on_another_cpu() {
        let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
        let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
        let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
        for cpu in [&mut cpu0, &mut cpu1] {
            system
                .register_idle_thread(
                    cpu.as_mut(),
                    ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
                )
                .unwrap();
            system.bring_cpu_online(cpu.as_mut()).unwrap();
        }
        let thread = system
            .create_thread(ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.make_ready(thread.id()).unwrap();
        system.enqueue(cpu0.as_mut(), thread.id(), 0).unwrap();

        // Model a stale remote publication reaching this owner while the
        // physical switch tail still proves that the same context executes on
        // another CPU. Selection must reject the contradiction instead of
        // overwriting the sole `on_cpu` authority.
        system
            .state
            .lock()
            .thread_record_mut(thread.id())
            .unwrap()
            .on_cpu = Some(CpuId::new(1));

        assert!(matches!(
            system.schedule(cpu0.as_mut(), 1),
            Err(TaskError::InvalidConfiguration)
        ));
    }

    #[test]
    fn owner_current_affinity_change_does_not_publish_a_self_request() {
        let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
        let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
        let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
        let running = system
            .install_bootstrap_thread(cpu0.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        for cpu in [&mut cpu0, &mut cpu1] {
            system
                .register_idle_thread(
                    cpu.as_mut(),
                    ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
                )
                .unwrap();
            system.bring_cpu_online(cpu.as_mut()).unwrap();
        }

        let mut target_only = CpuSet::empty(2);
        target_only.insert(CpuId::new(1));
        assert!(
            system
                .set_current_affinity(cpu0.as_mut(), target_only)
                .unwrap()
        );
        assert!(
            !cpu0.has_remote_work(),
            "the owner can commit its migration directly at the next schedule-out"
        );
        assert_eq!(system.thread_state(running.id()), Ok(ThreadState::Running));
    }

    #[test]
    fn initial_placement_hands_affinity_pinned_thread_to_its_owner_cpu() {
        let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
        let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
        let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
        system.bring_cpu_online(cpu0.as_mut()).unwrap();
        system.bring_cpu_online(cpu1.as_mut()).unwrap();

        let mut cpu1_only = CpuSet::empty(2);
        cpu1_only.insert(CpuId::new(1));
        let thread = system
            .create_thread(ThreadSpec::new(SchedulePolicy::default()).with_affinity(cpu1_only))
            .unwrap();
        system.make_ready(thread.id()).unwrap();

        system.place_ready(cpu0.as_mut(), thread.id(), 0).unwrap();
        assert!(cpu1.has_remote_work());
        system.drain_policy_updates(cpu1.as_mut(), 0).unwrap();
        assert_eq!(
            system.schedule(cpu1.as_mut(), 0).unwrap().next(),
            thread.id()
        );
    }

    #[test]
    fn class_order_is_deadline_then_rt_then_fair() {
        let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let policies = [
            SchedulePolicy::fair(Nice::ZERO, FairMode::Normal),
            SchedulePolicy::fifo(RtPriority::new(1).unwrap()),
            SchedulePolicy::deadline(DeadlinePolicy::new(1, 2, 3, DeadlineFlags::NONE).unwrap()),
        ];
        let mut ids = Vec::new();
        for policy in policies {
            let thread = system.create_thread(ThreadSpec::new(policy)).unwrap();
            system.make_ready(thread.id()).unwrap();
            system.enqueue(cpu.as_mut(), thread.id(), 0).unwrap();
            ids.push(thread.id());
        }
        assert_eq!(system.schedule(cpu.as_mut(), 0).unwrap().next(), ids[2]);
    }

    #[test]
    fn deadline_affinity_must_cover_online_root_domain() {
        let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
        let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
        let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
        system.bring_cpu_online(cpu0.as_mut()).unwrap();
        system.bring_cpu_online(cpu1.as_mut()).unwrap();
        let mut affinity = CpuSet::empty(2);
        affinity.insert(CpuId::new(0));
        let policy =
            SchedulePolicy::deadline(DeadlinePolicy::new(1, 2, 3, DeadlineFlags::NONE).unwrap());
        assert!(matches!(
            system.create_thread(ThreadSpec::new(policy).with_affinity(affinity)),
            Err(TaskError::DeadlineAffinity)
        ));
    }

    #[test]
    fn active_sleep_timer_pins_affinity_placement_to_its_owner_cpu() {
        let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
        let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
        let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
        system.bring_cpu_online(cpu0.as_mut()).unwrap();
        system.bring_cpu_online(cpu1.as_mut()).unwrap();
        let thread = system
            .create_thread(ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        thread.core.register_sleep_timer(CpuId::new(1), 7);

        let mut excludes_owner = CpuSet::empty(2);
        excludes_owner.insert(CpuId::new(0));
        assert_eq!(
            system.set_affinity(thread.id(), excludes_owner),
            Err(TaskError::ActiveTimerAffinity)
        );

        let mut includes_owner = CpuSet::empty(2);
        includes_owner.insert(CpuId::new(1));
        system.set_affinity(thread.id(), includes_owner).unwrap();
        assert_eq!(thread.wake_handle().target_cpu(), Some(CpuId::new(1)));
        assert!(thread.core.complete_sleep_timer(7));
    }

    #[test]
    fn queued_pi_owner_is_requeued_only_by_its_owner_cpu() {
        let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let owner = system
            .create_thread(ThreadSpec::new(SchedulePolicy::fair(
                Nice::new(19).unwrap(),
                FairMode::Normal,
            )))
            .unwrap();
        let competitor = system
            .create_thread(ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        let waiter = system
            .create_thread(ThreadSpec::new(SchedulePolicy::fifo(
                RtPriority::new(99).unwrap(),
            )))
            .unwrap();
        for thread in [&owner, &competitor] {
            system.make_ready(thread.id()).unwrap();
            system.enqueue(cpu.as_mut(), thread.id(), 0).unwrap();
        }
        let lock = PiLockId::new(1);

        let _wait = system.pi_wait_start(lock, waiter.id(), owner.id()).unwrap();

        assert!(matches!(
            owner.effective_policy(),
            SchedulePolicy::Fifo { priority } if priority.get() == 99
        ));
        assert_eq!(system.snapshot(cpu.as_ref()).runnable(), 2);
        let drain = system.drain_policy_updates(cpu.as_mut(), 1).unwrap();
        assert_eq!(drain.drained(), 1);
        assert_eq!(system.schedule(cpu.as_mut(), 1).unwrap().next(), owner.id());
    }

    #[test]
    fn chained_and_multi_lock_donations_are_withdrawn_independently() {
        let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
        let first_owner = system
            .create_thread(ThreadSpec::new(SchedulePolicy::fair(
                Nice::new(19).unwrap(),
                FairMode::Normal,
            )))
            .unwrap();
        let second_owner = system
            .create_thread(ThreadSpec::new(SchedulePolicy::fair(
                Nice::new(10).unwrap(),
                FairMode::Normal,
            )))
            .unwrap();
        let urgent = system
            .create_thread(ThreadSpec::new(SchedulePolicy::fifo(
                RtPriority::new(99).unwrap(),
            )))
            .unwrap();
        let first_lock = PiLockId::new(11);
        let second_lock = PiLockId::new(12);
        let chained = system
            .pi_wait_start(first_lock, second_owner.id(), first_owner.id())
            .unwrap();
        let urgent_wait = system
            .pi_wait_start(second_lock, urgent.id(), second_owner.id())
            .unwrap();
        assert!(matches!(
            first_owner.effective_policy(),
            SchedulePolicy::Fifo { priority } if priority.get() == 99
        ));

        system.pi_wait_cancel(urgent_wait).unwrap();
        assert_eq!(second_owner.effective_policy(), second_owner.policy());
        assert_eq!(first_owner.effective_policy(), second_owner.policy());

        system.pi_wait_cancel(chained).unwrap();
        assert_eq!(first_owner.effective_policy(), first_owner.policy());
    }

    #[test]
    fn deadline_donor_budget_is_debited_and_overrun_callback_is_deferred() {
        DEADLINE_OVERRUN_CALLBACKS.store(0, Ordering::Relaxed);
        let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let owner = system
            .create_thread(ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        let deadline = SchedulePolicy::deadline(
            DeadlinePolicy::new(10, 20, 100, DeadlineFlags::DL_OVERRUN).unwrap(),
        );
        let extension = unsafe { ThreadExtension::new(0, &DEADLINE_TEST_EXTENSION_OPS) };
        let donor = system
            .create_thread(ThreadSpec::new(deadline).with_extension(extension))
            .unwrap();
        let lock = PiLockId::new(21);
        for thread in [&owner, &donor] {
            system.make_ready(thread.id()).unwrap();
            system.enqueue(cpu.as_mut(), thread.id(), 0).unwrap();
        }
        assert_eq!(system.schedule(cpu.as_mut(), 0).unwrap().next(), donor.id());
        let _wait = system.pi_wait_start(lock, donor.id(), owner.id()).unwrap();
        system.drain_policy_updates(cpu.as_mut(), 0).unwrap();
        assert_eq!(
            system.block_current(cpu.as_mut()).unwrap().next(),
            owner.id()
        );

        let charged = system.charge_current(cpu.as_mut(), 10, 10, 0).unwrap();
        assert!(!charged.slice_expired());
        assert!(charged.deadline_overrun());
        assert_eq!(DEADLINE_OVERRUN_CALLBACKS.load(Ordering::Relaxed), 0);
        system.schedule(cpu.as_mut(), 10).unwrap();

        let donor_runtime = system.deadline_runtime(donor.id()).unwrap();
        assert_eq!(donor_runtime.remaining_runtime_ns(), 0);
        assert_eq!(donor_runtime.overruns(), 1);
        let owner_runtime = system.deadline_runtime(owner.id()).unwrap();
        assert_eq!(owner_runtime.donor(), Some(donor.id()));
        assert!(owner_runtime.pi_critical_rescue());
        assert_eq!(system.dispatch_deadline_overruns(1), 1);
        assert_eq!(DEADLINE_OVERRUN_CALLBACKS.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn wake_before_park_is_consumed_without_blocking() {
        let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let running = system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());

        assert_eq!(running.wake_handle().wake(), crate::WakeResult::Notified);

        assert_eq!(
            system.prepare_park(cpu.as_mut()).unwrap(),
            ParkPrepare::Notified
        );
        assert_eq!(
            system.thread_state(running.id()).unwrap(),
            ThreadState::Running
        );
        let wake = system.drain_remote_wakes(cpu.as_mut(), 0).unwrap();
        assert_eq!(wake.drained(), 1);
        assert!(!wake.pending());
    }

    #[test]
    fn consumed_running_wake_does_not_notify_a_later_park() {
        let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let running = system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());

        assert_eq!(running.wake_handle().wake(), crate::WakeResult::Notified);
        assert_eq!(
            system
                .drain_remote_wakes(cpu.as_mut(), 0)
                .unwrap()
                .drained(),
            1,
        );
        assert_eq!(
            system.thread_state(running.id()).unwrap(),
            ThreadState::Running,
        );
        assert!(matches!(
            system.prepare_park(cpu.as_mut()).unwrap(),
            ParkPrepare::Prepared(_),
        ));
    }

    #[test]
    fn wake_during_parking_cancels_schedule_out() {
        let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let running = system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        let ParkPrepare::Prepared(park) = system.prepare_park(cpu.as_mut()).unwrap() else {
            panic!("fresh park must publish PARKING");
        };

        assert_eq!(running.wake_handle().wake(), crate::WakeResult::Notified);

        assert!(matches!(
            system.commit_park(cpu.as_mut(), park).unwrap(),
            ParkCommit::Notified
        ));
        assert_eq!(
            system.thread_state(running.id()).unwrap(),
            ThreadState::Running
        );
    }

    #[test]
    fn drained_remote_wake_during_parking_is_committed_by_the_owner_thread() {
        let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let running = system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        let ParkPrepare::Prepared(park) = system.prepare_park(cpu.as_mut()).unwrap() else {
            panic!("fresh park must publish PARKING");
        };

        assert_eq!(running.wake_handle().wake(), crate::WakeResult::Notified);
        assert_eq!(
            system
                .drain_remote_wakes(cpu.as_mut(), 0)
                .unwrap()
                .drained(),
            1
        );
        assert_eq!(
            system.thread_state(running.id()).unwrap(),
            ThreadState::Parking,
            "the owner must finish a PARKING handshake before wake can enqueue it"
        );
        assert_eq!(system.snapshot(cpu.as_ref()).runnable(), 0);
        assert!(system.snapshot(cpu.as_ref()).need_resched());
        assert!(
            system
                .schedule_if_requested(cpu.as_mut(), 0)
                .unwrap()
                .parking_deferred(),
            "IRQ-return scheduling must defer while current owns a PARKING token"
        );
        assert_eq!(
            system.thread_state(running.id()).unwrap(),
            ThreadState::Parking
        );
        assert!(!system.snapshot(cpu.as_ref()).need_resched());

        assert!(matches!(
            system.commit_park(cpu.as_mut(), park).unwrap(),
            ParkCommit::Notified
        ));
        assert_eq!(
            system.thread_state(running.id()).unwrap(),
            ThreadState::Running
        );
        assert_eq!(system.snapshot(cpu.as_ref()).runnable(), 0);
        assert!(!system.snapshot(cpu.as_ref()).need_resched());
        assert!(
            matches!(
                system.schedule_if_requested(cpu.as_mut(), 0).unwrap(),
                SchedulerOutcome::Quiescent
            ),
            "a work-only wake must not be upgraded into a preemption"
        );
        assert_eq!(system.snapshot(cpu.as_ref()).current(), Some(running.id()));
        assert_eq!(system.snapshot(cpu.as_ref()).runnable(), 0);
        assert!(!system.snapshot(cpu.as_ref()).need_resched());
    }

    static DEADLINE_TEST_EXTENSION_OPS: ThreadExtensionOps = ThreadExtensionOps {
        on_switch_in: no_extension_hook,
        on_switch_out: no_extension_switch_out,
        on_exit: no_extension_hook,
        on_deadline_overrun: count_deadline_overrun,
        drop: no_extension_drop,
    };

    static EXIT_CALLBACK_INVOCATIONS: AtomicUsize = AtomicUsize::new(0);

    static EXIT_CALLBACK_TEST_OPS: ThreadExtensionOps = ThreadExtensionOps {
        on_switch_in: no_extension_hook,
        on_switch_out: no_extension_switch_out,
        on_exit: count_exit_callback,
        on_deadline_overrun: no_extension_hook,
        drop: no_extension_drop,
    };

    unsafe extern "Rust" fn no_extension_hook(_data: usize, _thread: ThreadId) {}

    unsafe extern "Rust" fn no_extension_switch_out(
        _data: usize,
        _thread: ThreadId,
        _reason: SwitchReason,
    ) {
    }

    unsafe extern "Rust" fn count_deadline_overrun(_data: usize, _thread: ThreadId) {
        DEADLINE_OVERRUN_CALLBACKS.fetch_add(1, Ordering::Relaxed);
    }

    unsafe extern "Rust" fn count_exit_callback(_data: usize, _thread: ThreadId) {
        EXIT_CALLBACK_INVOCATIONS.fetch_add(1, Ordering::Release);
    }

    unsafe extern "Rust" fn no_extension_drop(_data: usize) {}
}
