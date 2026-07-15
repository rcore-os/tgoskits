//! Generation-checked registry and scheduling orchestration.

use alloc::{sync::Arc, vec::Vec};
use core::{
    pin::Pin,
    ptr,
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
};

use super::thread_sched::{DeadlineActivity, ThreadSchedCell, ThreadSchedState};
use crate::{
    CpuId, CpuLocal, CpuRemote, CpuSet, CpuSnapshot, DeadlineAdmission, DeadlineEntity,
    EnqueueReason, FairMode, ParkCommit, ParkPrepare, ParkToken, PiLockId, PiWaitToken,
    QueuedThread, SchedulePolicy, SchedulingClass, SchedulingEntity, SwitchReason, TaskError,
    TaskSystemConfig, ThreadCore, ThreadExtension, ThreadExtensionBorrow, ThreadExtensionLease,
    ThreadExtensionView, ThreadHandle, ThreadId, ThreadLifecycle, ThreadResources,
    ThreadRuntimeSnapshot, ThreadSpec, ThreadState, ThreadWakeHandle,
    inbox::{InboxKind, InboxMessage, PublishResult, SchedulerInbox},
    lock::{IrqTicketLock, SequenceCounter},
    reclaim::DeferredReclaimNode,
    runtime::{
        ContextThreadBinding, ExecutionContextHandle, RuntimeStatus, ThreadIdentityV1, task_runtime,
    },
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
    // Cold-path order is registry/PI/admission -> root domain -> thread cell.
    // Owner runqueue progress may lock only its CpuLocal and thread cells.
    state: IrqTicketLock<TaskSystemState>,
    root_domain: IrqTicketLock<RootDomainState>,
    deferred_reclaims: SchedulerInbox,
    topology_sequence: SequenceCounter,
    online_count: AtomicUsize,
    pending_deadline_admission_release: AtomicU64,
}

#[derive(Debug)]
struct TaskSystemState {
    cpus: Vec<CpuRegistration>,
    slots: Vec<ThreadSlot>,
    free_slots: Vec<u32>,
    deadline_admission: DeadlineAdmission,
}

#[derive(Debug)]
struct RootDomainState {
    online: CpuSet,
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
    fn drain_pending_deadline_admission(&self, state: &mut TaskSystemState) {
        let released = self
            .pending_deadline_admission_release
            .swap(0, Ordering::AcqRel);
        state.deadline_admission.release(u128::from(released));
    }

    fn defer_deadline_admission_release(&self, released: u64) -> Result<(), TaskError> {
        self.pending_deadline_admission_release
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |pending| {
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
                cpus: cpu_registrations,
                slots: Vec::new(),
                free_slots: Vec::new(),
                deadline_admission: DeadlineAdmission::new(config.deadline_cap_percent()),
            }),
            root_domain: IrqTicketLock::new(RootDomainState {
                online: CpuSet::empty(config.cpu_count()),
            }),
            deferred_reclaims: SchedulerInbox::new(InboxKind::Reclaim),
            topology_sequence: SequenceCounter::default(),
            online_count: AtomicUsize::new(0),
            pending_deadline_admission_release: AtomicU64::new(0),
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

    fn ensure_owner_cpu_online(&self, cpu: &CpuLocal) -> Result<(), TaskError> {
        let remote = self
            .cpu_remotes
            .get(cpu.owner().as_usize())
            .ok_or(TaskError::InvalidCpu(cpu.owner().as_u32()))?;
        if Arc::ptr_eq(remote, cpu.remote()) && remote.is_online() {
            Ok(())
        } else {
            Err(TaskError::CpuOffline(cpu.owner().as_u32()))
        }
    }

    /// Completes CPU registration and publishes it in the online root domain.
    pub fn bring_cpu_online(&self, cpu: Pin<&mut CpuLocal>) -> Result<(), TaskError> {
        let id = cpu.owner();
        let mut state = self.state.lock();
        let mut root_domain = self.root_domain.lock();
        let registration = state.cpu_registration(id)?;
        if registration.online || cpu.is_online() {
            return Err(TaskError::CpuAlreadyOnline(id.as_u32()));
        }
        if !Arc::ptr_eq(&registration.remote, cpu.remote()) {
            return Err(TaskError::InvalidRuntimeHandle);
        }
        if state
            .slots
            .iter()
            .filter_map(|slot| slot.record.as_ref())
            .any(|record| {
                let sched = record.sched.lock();
                (matches!(sched.active_base_policy, SchedulePolicy::Deadline(_))
                    || matches!(sched.base_policy, SchedulePolicy::Deadline(_)))
                    && !sched.affinity.contains(id)
            })
        {
            return Err(TaskError::DeadlineAffinity);
        }
        self.topology_sequence.write_begin();
        state.cpu_registration_mut(id)?.online = true;
        cpu.as_ref().get_ref().remote().mark_online();
        root_domain.online.insert(id);
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
        self.drain_pending_deadline_admission(&mut state);
        let root_domain = self.root_domain.lock();
        let reservation = state.reserve_deadline(policy, &affinity, &root_domain.online)?;
        let (slot, generation) = state.allocate_thread_slot();
        let id = ThreadId::from_parts(slot, generation);
        let entity = SchedulingEntity::new(policy, self.config.fair_slice_ns(), 0);
        let base_deadline = match entity {
            SchedulingEntity::Deadline(deadline) => Some(deadline),
            _ => None,
        };
        let (extension, resources) = spec.into_owned_parts();
        let switch_extension = extension.as_ref().map(ThreadExtension::as_view);
        let sched = Arc::new(ThreadSchedCell::new(
            id,
            ThreadSchedState {
                lifecycle: ThreadLifecycle::new(),
                base_policy: policy,
                active_base_policy: policy,
                policy,
                policy_generation: 1,
                applied_policy_generation: 1,
                dispatch_generation: 1,
                affinity: affinity.clone(),
                entity,
                base_entity: entity,
                base_deadline,
                deadline_activity: DeadlineActivity::Inactive,
                deadline_bandwidth_cpu: None,
                deadline_cleanup_pending: false,
                deadline_bandwidth_scaled: u64::try_from(reservation).unwrap_or(u64::MAX),
                active_deadline_reservation: u64::try_from(reservation).unwrap_or(u64::MAX),
                desired_deadline_reservation: u64::try_from(reservation).unwrap_or(u64::MAX),
                deadline_zero_lag_ns: 0,
                queued_cpu: None,
                running_cpu: None,
                on_cpu: None,
                migration_target: None,
                blocked_pi_waiters: 0,
                pi_donor: None,
                deadline_donor: None,
                deadline_donor_core: None,
                deadline_cbs_borrower: None,
                deadline_cbs_generation: 1,
                pi_critical_rescue: false,
                deadline_replenish_pending: false,
                deadline_overrun_events: 0,
                charged_runtime_ns: 0,
                context: resources.context(),
                address_space: resources.address_space(),
            },
        ));
        let core = Arc::new(ThreadCore::new(
            id,
            policy,
            Arc::clone(&sched),
            switch_extension,
        ));
        let record = ThreadRecord {
            core: Arc::clone(&core),
            sched,
            extension,
            resources,
            blocked_on: None,
            exit_callback_pending: false,
            exit_callback_claimed: false,
        };
        let context = record.resources.context();
        if !context.is_none() {
            let status = task_runtime::bind_context_thread(ContextThreadBinding {
                context,
                identity: ThreadIdentityV1::new(id.slot(), id.generation()),
            });
            if status != RuntimeStatus::Success {
                let failed_slot = &mut state.slots[slot as usize];
                debug_assert!(failed_slot.record.is_none());
                failed_slot.generation = next_generation(failed_slot.generation);
                state.free_slots.push(slot);
                state.deadline_admission.release(reservation);
                drop(state);
                drop(record);
                return Err(TaskError::RuntimeFailure(status as u32));
            }
        }
        state.slots[slot as usize].record = Some(record);
        Ok(ThreadHandle { core })
    }

    /// Transitions a new or waking thread to `Ready`.
    pub fn make_ready(&self, thread: ThreadId) -> Result<(), TaskError> {
        let state = self.state.lock();
        let record = state.thread_record(thread)?;
        let mut sched = record.sched.lock();
        if sched.lifecycle.state() == ThreadState::Waking {
            let base_policy = sched.active_base_policy;
            sched.base_entity.reset_after_wake(base_policy);
            let effective_policy = sched.policy;
            sched.entity.reset_after_wake(effective_policy);
        }
        sched.transition(&record.core, ThreadState::Ready)
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
        let state = self.state.lock();
        state.cpu_registration(cpu.owner())?;
        if cpu.current().is_some() {
            return Err(TaskError::InvalidConfiguration);
        }
        let record = state.thread_record(thread.id())?;
        let core = Arc::clone(&record.core);
        let dispatch = {
            let mut sched = record.sched.lock();
            sched.transition(&core, ThreadState::Ready)?;
            sched.transition(&core, ThreadState::Running)?;
            sched.running_cpu = Some(cpu.owner());
            sched.on_cpu = Some(cpu.owner());
            core.set_target_cpu(cpu.owner());
            Self::owner_dispatch(&core, &sched, task_runtime::monotonic_ns())?
        };
        cpu.as_mut().set_current_core(Arc::clone(&core));
        cpu.as_mut().install_dispatch(dispatch);
        drop(state);
        self.publish_owner_cpu_load_summary(cpu.as_mut());
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
        let core = Arc::clone(&state.thread_record(thread.id())?.core);
        cpu.as_mut().set_idle(thread.id(), core);
        Ok(thread)
    }

    /// Enqueues a ready thread on an affinity-compatible owner CPU.
    pub fn enqueue(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        thread: ThreadId,
        now_ns: u64,
    ) -> Result<(), TaskError> {
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
                    if sched.queued_cpu.is_some()
                        || sched.running_cpu.is_some()
                        || sched.on_cpu.is_some()
                    {
                        return Err(TaskError::AlreadyQueued);
                    }
                    sched.migration_target = Some(target);
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
        sched.queued_cpu = None;
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
                    core.sched().lock().migration_target = Some(target);
                    self.publish_owner_migration(&core, target, owner, target)?;
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
                let _migrated = self.transfer_owner_balance_candidate(
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
            if source == target {
                let cleanup_deadline_member = {
                    let sched = core.sched().lock();
                    sched.deadline_cleanup_pending
                        && sched.deadline_bandwidth_cpu == Some(owner)
                        && sched.queued_cpu.is_none()
                        && sched.running_cpu.is_none()
                        && sched.on_cpu.is_none()
                };
                if cleanup_deadline_member {
                    Self::detach_owner_deadline_bandwidth(&core, cpu.as_mut())?;
                    core.sched().lock().deadline_cleanup_pending = false;
                    continue;
                }
            }
            if source != target {
                if target == owner {
                    let latest_target = core.sched().lock().migration_target;
                    if latest_target != Some(target) {
                        if let Some(latest_target) = latest_target {
                            // A second affinity update can overtake an already
                            // published transfer. Forward the detached message
                            // to the newest target; the embedded node is free
                            // again after this inbox batch detached it.
                            core.set_target_cpu(latest_target);
                            self.publish_owner_migration(
                                &core,
                                latest_target,
                                owner,
                                latest_target,
                            )?;
                        }
                        continue;
                    }
                    {
                        let mut sched = core.sched().lock();
                        if sched.lifecycle.state() != ThreadState::Ready
                            || sched.queued_cpu.is_some()
                            || sched.running_cpu.is_some()
                            || sched.on_cpu.is_some()
                        {
                            return Err(TaskError::InvalidConfiguration);
                        }
                        sched.migration_target = None;
                        core.set_target_cpu(owner);
                    }
                    self.enqueue_owner_thread(
                        cpu.as_mut(),
                        Arc::clone(&core),
                        now_ns,
                        EnqueueReason::Migrated,
                    )?;
                } else if source == owner {
                    let (queued_cpu, running_cpu, lifecycle, latest_target) = {
                        let sched = core.sched().lock();
                        (
                            sched.queued_cpu,
                            sched.running_cpu,
                            sched.lifecycle.state(),
                            sched.migration_target,
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
                        Self::detach_owner_deadline_bandwidth(&core, cpu.as_mut())?;
                        {
                            let mut sched = core.sched().lock();
                            sched.entity = queued.entity;
                            if !sched.is_pi_boosted() {
                                sched.base_entity = queued.entity;
                            }
                            sched.queued_cpu = None;
                            core.set_target_cpu(latest_target);
                        }
                        self.publish_owner_cpu_load_summary(cpu.as_mut());
                        self.publish_owner_migration(&core, latest_target, source, latest_target)?;
                    } else if running_cpu == Some(owner) {
                        cpu.request_reschedule();
                    } else if matches!(
                        lifecycle,
                        ThreadState::New
                            | ThreadState::Parking
                            | ThreadState::Blocked
                            | ThreadState::Waking
                    ) {
                        core.set_target_cpu(latest_target);
                        core.sched().lock().migration_target = None;
                    } else {
                        core.set_target_cpu(latest_target);
                        self.publish_owner_migration(&core, latest_target, source, latest_target)?;
                    }
                }
                continue;
            }
            let (queued_cpu, running_cpu, policy_generation, cbs_borrowed) = {
                let sched = core.sched().lock();
                (
                    sched.queued_cpu,
                    sched.running_cpu,
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
                    sched.queued_cpu = None;
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
                Self::commit_owner_current_dispatch(cpu.as_mut(), now_ns)?;
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
        if task_runtime::in_hard_irq() {
            return Ok(false);
        }
        self.ensure_owner_cpu_online(&cpu)?;
        if cpu.runnable_summary() != 0 {
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
        if task_runtime::in_hard_irq() {
            return Ok(None);
        }
        self.ensure_owner_cpu_online(&cpu)?;
        let source = cpu.owner();
        self.publish_owner_cpu_load_summary(cpu.as_mut());
        let source_summary = cpu.load_summary();
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
                let target_summary = remote.load_summary();
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
        self.complete_context_switch(cpu.as_mut())?;
        self.drain_owner_work(cpu.as_mut(), now_ns)?;
        self.ensure_owner_cpu_online(&cpu)?;
        cpu.as_mut().scheduler_enter();
        Self::commit_owner_current_dispatch(cpu.as_mut(), now_ns)?;
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
        let next_core = self.pick_owner_next(cpu.as_mut(), now_ns, previous)?;
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
        Self::commit_owner_current_dispatch(cpu.as_mut(), now_ns)?;
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
        let next_core = self.pick_owner_next(cpu.as_mut(), now_ns, previous)?;
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
        self.ensure_owner_cpu_online(&cpu)?;
        cpu.as_mut().scheduler_enter();
        Self::commit_owner_current_dispatch(cpu.as_mut(), now_ns)?;
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
                    sched.running_cpu = None;
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
        let next_core = self.pick_owner_next(cpu.as_mut(), now_ns, previous)?;
        Self::stage_switch_handoff(
            cpu.as_mut(),
            previous,
            previous_core.as_ref().map(Arc::clone),
            next_core.id(),
            migration_target,
        )?;
        let decision =
            Self::owner_switch_plan(previous_core.as_ref(), &next_core, SwitchReason::Yield);
        Self::program_local_timer(cpu.as_mut(), now_ns)?;
        self.balance_after_schedule(cpu.as_mut(), decision.next(), now_ns)?;
        Ok(decision)
    }

    /// Publishes `PARKING` after consuming a wake-before-park notification.
    pub fn prepare_park(&self, mut cpu: Pin<&mut CpuLocal>) -> Result<ParkPrepare, TaskError> {
        self.complete_context_switch(cpu.as_mut())?;
        self.ensure_owner_cpu_online(&cpu)?;
        let core = cpu.current_core().ok_or(TaskError::NoRunnableThread)?;
        if core.take_park_notification() {
            return Ok(ParkPrepare::Notified);
        }
        let generation = core.next_park_generation();
        core.sched().lock().transition(core, ThreadState::Parking)?;
        Ok(ParkPrepare::Prepared(ParkToken::new(core.id(), generation)))
    }

    /// Rechecks a prepared park and either cancels it or commits schedule-out.
    pub fn commit_park(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        token: ParkToken,
    ) -> Result<ParkCommit, TaskError> {
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
            return Ok(ParkCommit::Notified);
        }
        cpu.as_mut().scheduler_enter();
        cpu.finish_park_preemption(false);
        Self::commit_owner_current_dispatch(cpu.as_mut(), now_ns)?;
        {
            let mut sched = previous_core.sched().lock();
            sched.transition(&previous_core, ThreadState::Blocked)?;
            sched.running_cpu = None;
        }
        Self::mark_owner_deadline_non_contending(&previous_core, cpu.as_mut(), now_ns)?;
        cpu.as_mut().clear_current();
        let next_core = self.pick_owner_next(cpu.as_mut(), now_ns, Some(token.thread()))?;
        Self::stage_switch_handoff(
            cpu.as_mut(),
            Some(token.thread()),
            Some(Arc::clone(&previous_core)),
            next_core.id(),
            None,
        )?;
        Ok(ParkCommit::Blocked(Self::owner_switch_plan(
            Some(&previous_core),
            &next_core,
            SwitchReason::Blocked,
        )))
    }

    /// Cancels a prepared park because an independent grant won the race.
    pub fn cancel_park(&self, cpu: Pin<&mut CpuLocal>, token: ParkToken) -> Result<(), TaskError> {
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
                    let core = cpu.current_core().ok_or(TaskError::NoRunnableThread)?;
                    Ok(Self::owner_switch_plan(
                        Some(core),
                        core,
                        SwitchReason::Blocked,
                    ))
                }
            },
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
        if record.blocked_on.is_some() || sched.blocked_pi_waiters != 0 {
            return Err(TaskError::InvalidPiState);
        }
        if sched.running_cpu != Some(cpu.owner()) || sched.on_cpu != Some(cpu.owner()) {
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
            let previous_core = cpu.current_core().cloned();
            if state.thread_record(previous)?.has_live_pi_edges() {
                return Err(TaskError::InvalidPiState);
            }
            cpu.as_mut().scheduler_enter();
            Self::commit_owner_current_dispatch(cpu.as_mut(), now_ns)?;
            let previous_core = previous_core.ok_or(TaskError::NoRunnableThread)?;
            Self::detach_owner_deadline_bandwidth(&previous_core, cpu.as_mut())?;
            {
                let mut sched = previous_core.sched().lock();
                sched.transition(&previous_core, ThreadState::Exited)?;
                sched.running_cpu = None;
                let record = state.thread_record_mut(previous)?;
                record.exit_callback_pending = record.extension.is_some();
                record.exit_callback_claimed = false;
            }
            state.release_deadline_reservation_on_exit(previous)?;
            cpu.as_mut().clear_current();
            let next_core = self.pick_owner_next(cpu.as_mut(), now_ns, Some(previous))?;
            Self::stage_switch_handoff(
                cpu.as_mut(),
                Some(previous),
                Some(Arc::clone(&previous_core)),
                next_core.id(),
                None,
            )?;
            Self::owner_switch_plan(Some(&previous_core), &next_core, SwitchReason::Exited)
        };
        Ok(decision)
    }

    /// Completes the physical switch-out handoff in the newly active context.
    ///
    /// This second phase clears `on_cpu` only after architecture execution has
    /// left the previous stack. Deferred migration publication and exit hooks
    /// therefore cannot make a context runnable or reapable too early.
    pub fn complete_context_switch(&self, mut cpu: Pin<&mut CpuLocal>) -> Result<(), TaskError> {
        let Some(expected_handoff) = cpu.as_ref().get_ref().switch_handoff() else {
            return Ok(());
        };
        let expected_previous = expected_handoff.previous.id();
        let expected_migration_target = expected_handoff.migration_target;
        ensure_runtime_success(task_runtime::finish_context_switch_tail())?;
        let handoff = cpu
            .as_mut()
            .take_switch_handoff()
            .ok_or(TaskError::InvalidConfiguration)?;
        if handoff.previous.id() != expected_previous
            || handoff.migration_target != expected_migration_target
        {
            return Err(TaskError::InvalidConfiguration);
        }
        let previous = handoff.previous.id();
        let migration_target = {
            let mut sched = handoff.previous.sched().lock();
            if sched.on_cpu != Some(cpu.owner()) {
                return Err(TaskError::InvalidConfiguration);
            }
            sched.on_cpu = None;
            match handoff.migration_target {
                Some(_) => {
                    let target = sched
                        .migration_target
                        .ok_or(TaskError::InvalidConfiguration)?;
                    if sched.lifecycle.state() != ThreadState::Ready
                        || sched.queued_cpu.is_some()
                        || sched.running_cpu.is_some()
                    {
                        return Err(TaskError::InvalidConfiguration);
                    }
                    handoff.previous.set_target_cpu(target);
                    Some(target)
                }
                None => None,
            }
        };
        if let Some(target) = migration_target {
            Self::detach_owner_deadline_bandwidth(&handoff.previous, cpu.as_mut())?;
            self.publish_owner_migration(&handoff.previous, target, cpu.owner(), target)?;
        }
        debug_assert_eq!(previous, handoff.previous.id());
        self.publish_owner_cpu_load_summary(cpu.as_mut());
        Ok(())
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
        sched.queued_cpu = Some(owner);
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
        mut cpu: Pin<&mut CpuLocal>,
    ) -> Result<(), TaskError> {
        let owner = cpu.owner();
        let mut sched = core.sched().lock();
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
                    let cbs_available =
                        donor_sched.running_cpu.is_none() && donor_sched.queued_cpu.is_none();
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
            donor.deadline_cbs_borrower = None;
            donor.deadline_cbs_generation = next_cbs_generation;
        }
        let mut sched = dispatch.runtime_core_arc().sched().lock();
        sched.charged_runtime_ns = sched
            .charged_runtime_ns
            .saturating_add(dispatch.charged_runtime_ns());
        if sched.dispatch_generation != dispatch.policy_generation {
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
            }
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
        self.state
            .lock()
            .recompute_pi_chain(thread, self.config.fair_slice_ns())
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
        let pointer = Arc::as_ptr(core);
        // SAFETY: this count is transferred to the embedded inbox node and
        // consumed by exactly one later owner drain.
        unsafe { Arc::increment_strong_count(pointer) };
        // SAFETY: the transferred Arc count keeps the embedded node pinned.
        let node = unsafe { Pin::new_unchecked((*pointer).policy_update_node()) };
        let message = InboxMessage::migration_with_payload(
            core.id(),
            owner,
            owner,
            generation,
            pointer.expose_provenance(),
        );
        if remote.publish_policy_update(node, message) != PublishResult::Published {
            // SAFETY: rejected/coalesced publication did not consume this
            // attempt's retained reference.
            unsafe { Arc::decrement_strong_count(pointer) };
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
        let (source, remains_placed) = {
            sched.affinity = affinity;
            let location = sched.running_cpu.or(sched.queued_cpu);
            let source = match location {
                Some(owner) if !sched.affinity.contains(owner) => {
                    sched.migration_target = Some(target);
                    Some(owner)
                }
                Some(owner) => {
                    // A newer mask made the owner legal again before its
                    // pending migration request ran. Cancel that request.
                    sched.migration_target = None;
                    core.set_target_cpu(owner);
                    None
                }
                None if sched.migration_target.is_some() => {
                    // The source already detached this ready thread and a
                    // transfer is in flight. Retarget the transfer in-place;
                    // the old destination forwards it after observing this
                    // state under the scheduler lock.
                    sched.migration_target = Some(target);
                    core.set_target_cpu(target);
                    None
                }
                None => {
                    core.set_target_cpu(target);
                    None
                }
            };
            (source, location.is_some())
        };
        drop(sched);
        if let Some(source) = source {
            state.publish_migration_request(&core, source, target)?;
        } else if remains_placed {
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
        let state = self.state.lock();
        let root_domain = self.root_domain.lock();
        state.ensure_cpu_online(&cpu)?;
        let current = cpu.current().ok_or(TaskError::NoRunnableThread)?;
        let record = state.thread_record(current)?;
        let mut sched = record.sched.lock();
        if sched.running_cpu != Some(cpu.owner()) || sched.on_cpu != Some(cpu.owner()) {
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
        sched.migration_target = must_migrate.then_some(target);
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
        let state = self.state.lock();
        state.cpu_registration(cpu.owner())?;
        let core = Arc::clone(&state.thread_record(thread)?.core);
        cpu.as_mut().set_idle(thread, core);
        Ok(())
    }

    /// Marks a non-queued thread exited and invokes its task-context exit hook.
    pub fn mark_exited(&self, thread: ThreadId) -> Result<(), TaskError> {
        let extension = {
            let mut state = self.state.lock();
            let cleanup_deadline_member = {
                let record = state.thread_record_mut(thread)?;
                let mut sched = record.sched.lock();
                if sched.queued_cpu.is_some() || sched.running_cpu.is_some() {
                    return Err(TaskError::AlreadyQueued);
                }
                if sched.on_cpu.is_some() {
                    return Err(TaskError::ThreadBusy);
                }
                if record.blocked_on.is_some() || sched.blocked_pi_waiters != 0 {
                    return Err(TaskError::InvalidPiState);
                }
                if sched.deadline_cbs_borrower.is_some() {
                    return Err(TaskError::ThreadBusy);
                }
                if sched.deadline_bandwidth_cpu.is_some() {
                    sched.deadline_cleanup_pending = true;
                    true
                } else {
                    false
                }
            };
            if cleanup_deadline_member {
                state.request_owner_reschedule(thread);
                return Err(TaskError::ThreadBusy);
            }
            let record = state.thread_record_mut(thread)?;
            let mut sched = record.sched.lock();
            sched.transition(&record.core, ThreadState::Exited)?;
            record.exit_callback_pending = record.extension.is_some();
            record.exit_callback_claimed = record.exit_callback_pending;
            let extension = record.extension.as_ref().map(ThreadExtension::as_view);
            drop(sched);
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
                || record.sched.lock().on_cpu.is_some()
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
        Ok(self
            .state
            .lock()
            .thread_record(thread)?
            .sched
            .lock()
            .lifecycle
            .state())
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
        debug_assert!(snapshot.charged_runtime_ns() >= record.sched.lock().charged_runtime_ns);
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
        let mut sched = record.sched.lock();
        if sched.lifecycle.state() != ThreadState::Running
            || sched.running_cpu != Some(owner)
            || sched.on_cpu != Some(owner)
            || sched.queued_cpu.is_some()
        {
            return Err(TaskError::InvalidConfiguration);
        }
        let previous = record.resources.replace_address_space(address_space);
        sched.address_space = address_space;
        Ok(previous)
    }

    /// Attempts a non-waiting state query.
    ///
    /// Returns `Ok(None)` when another CPU owns the registry critical section.
    pub fn try_thread_state(&self, thread: ThreadId) -> Result<Option<ThreadState>, TaskError> {
        let Some(state) = self.state.try_lock() else {
            return Ok(None);
        };
        Ok(Some(
            state.thread_record(thread)?.sched.lock().lifecycle.state(),
        ))
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
        Ok(handle.extension_view())
    }

    /// Returns the thread's effective/base scheduling policy snapshot.
    pub fn thread_policy(&self, thread: ThreadId) -> Result<SchedulePolicy, TaskError> {
        Ok(self
            .state
            .lock()
            .thread_record(thread)?
            .sched
            .lock()
            .base_policy)
    }

    /// Publishes a new base-policy generation for owner-CPU application.
    pub fn set_thread_policy(
        &self,
        thread: ThreadId,
        policy: SchedulePolicy,
    ) -> Result<(), TaskError> {
        policy.validate()?;
        let mut state = self.state.lock();
        self.drain_pending_deadline_admission(&mut state);
        let root_domain = self.root_domain.lock();
        let (core, sched_cell) = {
            let record = state.thread_record(thread)?;
            (Arc::clone(&record.core), Arc::clone(&record.sched))
        };
        let mut sched = sched_cell.lock();
        let active_reservation = u128::from(sched.active_deadline_reservation);
        let desired_reservation = u128::from(sched.desired_deadline_reservation);
        let affinity = sched.affinity.clone();
        let owner = sched
            .running_cpu
            .or(sched.queued_cpu)
            .or(sched.deadline_bandwidth_cpu);
        let generation = sched
            .policy_generation
            .checked_add(1)
            .ok_or(TaskError::InvalidConfiguration)?;
        let reservation = state.deadline_reservation_for(policy, &affinity, &root_domain.online)?;
        let old_held = active_reservation.max(desired_reservation);
        let new_held = active_reservation.max(reservation);
        if new_held > old_held {
            state
                .deadline_admission
                .reserve_utilization(new_held - old_held)?;
        } else {
            state.deadline_admission.release(old_held - new_held);
        }
        sched.desired_deadline_reservation = u64::try_from(reservation).unwrap_or(u64::MAX);
        sched.base_policy = policy;
        sched.policy_generation = generation;
        drop(sched);
        core.publish_base_policy(policy);
        if owner.is_some() {
            state.request_owner_reschedule(thread);
        } else {
            drop(root_domain);
            drop(state);
            let applied = self.apply_owner_policy_generation(
                &core,
                generation,
                task_runtime::monotonic_ns(),
                None,
                false,
            )?;
            if applied {
                self.recompute_pi_after_policy_update(thread)?;
            }
        }
        Ok(())
    }

    /// Returns a copy of the thread CPU affinity mask.
    pub fn thread_affinity(&self, thread: ThreadId) -> Result<CpuSet, TaskError> {
        Ok(self
            .state
            .lock()
            .thread_record(thread)?
            .sched
            .lock()
            .affinity
            .clone())
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

    /// Runs a bounded batch of deferred Deadline overrun callbacks.
    ///
    /// Timer IRQ only publishes pending state. This task-context operation drops
    /// the registry lock before invoking any OS extension callback.
    pub fn dispatch_deadline_overruns(&self, limit: usize) -> usize {
        let callbacks = {
            let mut state = self.state.lock();
            let mut callbacks = Vec::with_capacity(limit.min(state.slots.len()));
            let mut processed = 0;
            for slot in &mut state.slots {
                if processed == limit {
                    break;
                }
                let Some(record) = &mut slot.record else {
                    continue;
                };
                let mut sched = record.sched.lock();
                if sched.deadline_overrun_events == 0 {
                    continue;
                }
                let events = sched
                    .deadline_overrun_events
                    .min(u64::try_from(limit - processed).unwrap_or(u64::MAX));
                sched.deadline_overrun_events -= events;
                let events = usize::try_from(events).unwrap_or(limit - processed);
                processed += events;
                if let Some(extension) = record.extension.as_ref() {
                    callbacks.extend((0..events).map(|_| {
                        (
                            extension.as_view(),
                            record.core.id(),
                            Arc::clone(&record.core),
                        )
                    }));
                }
            }
            callbacks
        };
        for (extension, thread, _retained_core) in &callbacks {
            // SAFETY: the retained core keeps the extension's registry record
            // live and callbacks run only after releasing scheduler locks.
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
        if state.thread_record(waiter)?.sched.lock().lifecycle.state() == ThreadState::Exited
            || state.thread_record(owner)?.sched.lock().lifecycle.state() == ThreadState::Exited
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
            .sched
            .lock()
            .blocked_pi_waiters
            .checked_add(1)
            .ok_or(TaskError::InvalidPiState)?;
        let generation = waiter_core.pi_wait_state().begin()?;
        state.thread_record_mut(waiter)?.blocked_on = Some(PiWaitRegistration {
            lock,
            owner,
            generation,
        });
        state.thread_record(owner)?.sched.lock().blocked_pi_waiters = next_waiter_count;
        state.recompute_pi_chain(owner, self.config.fair_slice_ns())?;
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
        let owner = state.thread_record(registration.owner)?;
        let mut owner_sched = owner.sched.lock();
        owner_sched.blocked_pi_waiters = owner_sched
            .blocked_pi_waiters
            .checked_sub(1)
            .ok_or(TaskError::InvalidPiState)?;
        drop(owner_sched);
        state.recompute_pi_chain(registration.owner, self.config.fair_slice_ns())?;
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
                    .sched
                    .lock()
                    .blocked_pi_waiters
                    .checked_add(redirected_waiters)
                    .ok_or(TaskError::InvalidPiState)
            })
            .transpose()?;
        {
            let record = state.thread_record(old_owner)?;
            let mut sched = record.sched.lock();
            if sched.blocked_pi_waiters < active_waiters {
                return Err(TaskError::InvalidPiState);
            }
            sched.blocked_pi_waiters -= active_waiters;
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
            state.thread_record(next)?.sched.lock().blocked_pi_waiters =
                next_waiter_count.unwrap_or(0);
        }
        state.recompute_pi_chain(old_owner, self.config.fair_slice_ns())?;
        if let Some(next) = next_owner {
            state.recompute_pi_chain(next, self.config.fair_slice_ns())?;
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

    fn publish_owner_cpu_load_summary(&self, mut cpu: Pin<&mut CpuLocal>) {
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
                || sched.queued_cpu != Some(source)
                || sched.migration_target.is_some()
                || sched.on_cpu.is_some()
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
            if sched.lifecycle.state() != ThreadState::Ready || sched.queued_cpu != Some(source) {
                return Err(TaskError::InvalidConfiguration);
            }
            sched.entity = queued.entity;
            if !sched.is_pi_boosted() {
                sched.base_entity = queued.entity;
            }
            sched.queued_cpu = None;
            sched.migration_target = Some(target);
            core.set_target_cpu(target);
        }
        if matches!(candidate.policy, SchedulePolicy::Fair { .. }) {
            cpu.defer_fair_balance(now_ns, self.config.balance_interval_ns());
        }
        self.publish_owner_cpu_load_summary(cpu.as_mut());
        self.publish_owner_migration(&core, target, source, target)?;
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
                    if let Some(deadline) = sched.base_deadline {
                        cpu.arm_deferred_scheduler_deadline(
                            deadline
                                .next_scheduler_event_ns()
                                .max(now_ns.saturating_add(1)),
                        );
                    }
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
                    } else if !sched.is_pi_boosted() && sched.queued_cpu == Some(owner) {
                        update_queued = Some(SchedulingEntity::Deadline(deadline));
                    }
                } else if missed {
                    sched.base_deadline = Some(deadline);
                    sched.base_entity = SchedulingEntity::Deadline(deadline);
                    if !sched.is_pi_boosted() {
                        sched.entity = sched.base_entity;
                        if sched.queued_cpu == Some(owner) {
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
        self.ensure_owner_cpu_online(&cpu)?;
        self.publish_owner_cpu_load_summary(cpu.as_mut());
        let source = cpu.owner();
        let source_load = cpu.runnable_summary();
        let target = self
            .cpu_remotes
            .iter()
            .enumerate()
            .filter(|(index, remote)| remote.is_online() && CpuId::new(*index as u32) != source)
            .filter_map(|(index, remote)| {
                let target = CpuId::new(index as u32);
                let target_summary = remote.load_summary();
                if target_summary.runnable_count() >= source_load {
                    return None;
                }
                self.select_owner_balance_candidate(
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
            self.transfer_owner_balance_candidate(
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
            sched.migration_target.is_some() || !sched.affinity.contains(owner);
        if migration_requested {
            let target = sched
                .migration_target
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
            sched.migration_target = Some(target);
            sched.transition(&core, ThreadState::Ready)?;
            sched.running_cpu = None;
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
            sched.running_cpu = None;
            cpu.as_mut().clear_current();
            return Ok(None);
        }

        if cpu.idle() == Some(core.id()) {
            sched.transition(&core, ThreadState::Ready)?;
            sched.running_cpu = None;
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
        sched.running_cpu = None;
        let enqueue =
            self.enqueue_owner_thread_locked(cpu.as_mut(), &core, &mut sched, now_ns, reason);
        let preempts_current = match enqueue {
            Ok(preempts_current) => preempts_current,
            Err(error) => {
                sched.running_cpu = Some(owner);
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
                    .then(|| (remote.runnable_summary(), cpu))
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
        match sched.on_cpu {
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
    ) -> Result<Arc<ThreadCore>, TaskError> {
        let owner = cpu.owner();
        let fields = cpu.as_mut().fields_mut();
        let ordinary_rt_may_run = fields.rt_bandwidth.may_run(now_ns, false);
        let core = if let Some(queued) = fields
            .run_queue
            .pick_next_with_rt(ordinary_rt_may_run, |queued| {
                queued.core.sched().lock().is_pi_boosted_rt_owner()
            }) {
            let core = queued.core;
            {
                let mut sched = core.sched().lock();
                Self::validate_owner_next(&sched, core.id(), owner, outgoing)?;
                sched.entity = queued.entity;
                if !sched.is_pi_boosted() {
                    sched.base_entity = queued.entity;
                }
                sched.queued_cpu = None;
                sched.running_cpu = Some(owner);
                sched.on_cpu = Some(owner);
                sched.transition(&core, ThreadState::Running)?;
                let dispatch = Self::owner_dispatch(&core, &sched, now_ns)?;
                fields.current_dispatch = Some(dispatch);
            }
            core
        } else {
            let core = fields
                .idle_core
                .as_ref()
                .cloned()
                .ok_or(TaskError::NoRunnableThread)?;
            {
                let mut sched = core.sched().lock();
                Self::validate_owner_next(&sched, core.id(), owner, outgoing)?;
                if sched.lifecycle.state() == ThreadState::Ready {
                    sched.transition(&core, ThreadState::Running)?;
                }
                sched.running_cpu = Some(owner);
                sched.on_cpu = Some(owner);
                let dispatch = Self::owner_dispatch(&core, &sched, now_ns)?;
                fields.current_dispatch = Some(dispatch);
            }
            core
        };
        cpu.as_mut().set_current_core(Arc::clone(&core));
        self.publish_owner_cpu_load_summary(cpu.as_mut());
        Ok(core)
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

impl TaskSystemState {
    fn reserve_deadline(
        &mut self,
        policy: SchedulePolicy,
        affinity: &CpuSet,
        online: &CpuSet,
    ) -> Result<u128, TaskError> {
        match policy {
            SchedulePolicy::Deadline(deadline) => {
                if !affinity.covers(online) {
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
        online: &CpuSet,
    ) -> Result<u128, TaskError> {
        match policy {
            SchedulePolicy::Deadline(deadline) => {
                if !affinity.covers(online) {
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
            let record = self.thread_record(thread)?;
            let mut sched = record.sched.lock();
            let held = sched
                .active_deadline_reservation
                .max(sched.desired_deadline_reservation);
            sched.active_deadline_reservation = 0;
            sched.desired_deadline_reservation = 0;
            held
        };
        self.deadline_admission.release(u128::from(held));
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
            let sched = record.sched.lock();
            if sched.lifecycle.state() != ThreadState::Exited {
                return Err(TaskError::NotExited);
            }
            if sched.on_cpu.is_some()
                || sched.deadline_bandwidth_cpu.is_some()
                || sched.deadline_cleanup_pending
                || sched.deadline_cbs_borrower.is_some()
                || sched.deadline_overrun_events != 0
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
        let held = {
            let sched = record.sched.lock();
            sched
                .active_deadline_reservation
                .max(sched.desired_deadline_reservation)
        };
        self.deadline_admission.release(u128::from(held));
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
                let sched = record.sched.lock();
                if sched.lifecycle.state() != ThreadState::Exited
                    || sched.on_cpu.is_some()
                    || sched.deadline_bandwidth_cpu.is_some()
                    || sched.deadline_cleanup_pending
                    || sched.deadline_cbs_borrower.is_some()
                    || sched.deadline_overrun_events != 0
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
            let sched = record.sched.lock();
            if sched.lifecycle.state() != ThreadState::Exited
                || sched.on_cpu.is_some()
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
        let sched = record.sched.lock();
        if sched.lifecycle.state() != ThreadState::Exited
            || sched.on_cpu.is_some()
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
        if let Ok(record) = self.thread_record(owner) {
            let (cpu, generation) = {
                let sched = record.sched.lock();
                (
                    sched
                        .running_cpu
                        .or(sched.queued_cpu)
                        .or(sched.deadline_bandwidth_cpu),
                    sched.policy_generation,
                )
            };
            let Some(cpu) = cpu else {
                return;
            };
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
                generation,
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

    fn recompute_pi_chain(&mut self, start: ThreadId, fair_slice_ns: u64) -> Result<(), TaskError> {
        let mut current = start;
        for _ in 0..=self.slots.len() {
            let (
                current_core,
                base,
                base_entity,
                blocked_on,
                previous_policy,
                previous_entity,
                previous_pi_donor,
                previous_deadline_donor,
            ) = {
                let record = self.thread_record(current)?;
                let sched = record.sched.lock();
                let base_entity = sched
                    .base_deadline
                    .filter(|_| matches!(sched.active_base_policy, SchedulePolicy::Deadline(_)))
                    .map(SchedulingEntity::Deadline)
                    .unwrap_or(sched.base_entity);
                (
                    Arc::clone(&record.core),
                    sched.active_base_policy,
                    base_entity,
                    record.blocked_on,
                    sched.policy,
                    sched.entity,
                    sched.pi_donor,
                    sched.deadline_donor,
                )
            };
            let mut effective = base;
            let mut effective_entity = base_entity;
            let mut effective_urgency = base_entity.scheduling_urgency(base);
            let mut pi_donor = None;
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
                let (donor_policy, donor) = {
                    let sched = donor_record.sched.lock();
                    (sched.policy, sched.pi_donor.unwrap_or(waiter))
                };
                let donor_entity = if matches!(donor_policy, SchedulePolicy::Deadline(_)) {
                    self.thread_record(donor)?
                        .sched
                        .lock()
                        .base_deadline
                        .map(SchedulingEntity::Deadline)
                        .ok_or(TaskError::InvalidPiState)?
                } else if previous_pi_donor == Some(donor)
                    && previous_policy == donor_policy
                    && previous_entity.matches_policy(donor_policy)
                {
                    previous_entity
                } else {
                    let virtual_time = base_entity.fair().map_or(0, |fair| fair.vruntime());
                    SchedulingEntity::new(donor_policy, fair_slice_ns, virtual_time)
                };
                let donor_urgency = donor_entity.scheduling_urgency(donor_policy);
                if donor_urgency < effective_urgency {
                    effective = donor_policy;
                    effective_entity = donor_entity;
                    effective_urgency = donor_urgency;
                    pi_donor = Some(donor);
                    deadline_donor =
                        matches!(donor_policy, SchedulePolicy::Deadline(_)).then_some(donor);
                }
            }
            let changed = previous_policy != effective
                || previous_pi_donor != pi_donor
                || previous_deadline_donor != deadline_donor;
            let deadline_donor_core = deadline_donor
                .map(|donor| {
                    self.thread_record(donor)
                        .map(|record| Arc::downgrade(&record.core))
                })
                .transpose()?;
            let (rescue_changed, policy, entity) = {
                let mut sched = current_core.sched().lock();
                if changed {
                    sched.policy = effective;
                    sched.pi_donor = pi_donor;
                    sched.deadline_donor = deadline_donor;
                    sched.deadline_donor_core = deadline_donor_core;
                    sched.entity = effective_entity;
                }
                let should_rescue = sched.blocked_pi_waiters != 0
                    && sched
                        .entity
                        .deadline()
                        .is_some_and(|deadline| deadline.remaining_runtime_ns() == 0);
                let rescue_changed = should_rescue != sched.pi_critical_rescue;
                if rescue_changed {
                    sched.pi_critical_rescue = should_rescue;
                    if should_rescue {
                        sched.entity.enter_pi_critical_rescue();
                    } else {
                        sched.entity.leave_pi_critical_rescue();
                    }
                    if !sched.is_pi_boosted() {
                        sched.base_entity = sched.entity;
                        if let SchedulingEntity::Deadline(deadline) = sched.entity {
                            sched.base_deadline = Some(deadline);
                        }
                    }
                }
                if changed || rescue_changed {
                    sched.dispatch_generation = sched
                        .dispatch_generation
                        .checked_add(1)
                        .ok_or(TaskError::InvalidConfiguration)?;
                }
                (rescue_changed, sched.policy, sched.entity)
            };
            if changed || rescue_changed {
                current_core.publish_effective_schedule(policy, entity);
                self.request_owner_reschedule(current);
            }
            let Some(registration) = blocked_on else {
                return Ok(());
            };
            current = registration.owner;
        }
        Err(TaskError::PiCycle)
    }

    fn cpu_remote(&self, cpu: CpuId) -> Option<&CpuRemote> {
        let registration = self.cpu_registration(cpu).ok()?;
        if !registration.online || !registration.remote.is_online() {
            return None;
        }
        Some(registration.remote.as_ref())
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
    fn from_core(core: &ThreadCore) -> Self {
        let sched = core.sched().lock();
        Self {
            thread: core.id(),
            context: sched.context,
            address_space: sched.address_space,
            extension: core.extension_view(),
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
    sched: Arc<ThreadSchedCell>,
    extension: Option<ThreadExtension>,
    resources: ThreadResources,
    blocked_on: Option<PiWaitRegistration>,
    exit_callback_pending: bool,
    exit_callback_claimed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PiWaitRegistration {
    lock: PiLockId,
    owner: ThreadId,
    generation: u64,
}

impl ThreadRecord {
    fn has_live_pi_edges(&self) -> bool {
        self.blocked_on.is_some() || self.sched.lock().blocked_pi_waiters != 0
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
    fn cpu_owners_schedule_while_cold_domains_are_locked() {
        use std::{
            sync::{Barrier, mpsc},
            thread,
            time::Duration,
        };

        let system = Arc::new(TaskSystem::new(TaskSystemConfig::new(2)).unwrap());
        let ready = Arc::new(Barrier::new(3));
        let start = Arc::new(Barrier::new(3));
        let (completed, progress) = mpsc::channel();
        let mut workers = Vec::new();

        for cpu_index in 0..2 {
            let system = Arc::clone(&system);
            let ready = Arc::clone(&ready);
            let start = Arc::clone(&start);
            let completed = completed.clone();
            workers.push(thread::spawn(move || {
                let mut cpu = system.create_cpu_local(CpuId::new(cpu_index)).unwrap();
                system
                    .install_bootstrap_thread(
                        cpu.as_mut(),
                        ThreadSpec::new(SchedulePolicy::default()),
                    )
                    .unwrap();
                system.bring_cpu_online(cpu.as_mut()).unwrap();
                ready.wait();
                start.wait();
                let result = system
                    .drain_policy_updates(cpu.as_mut(), 1)
                    .and_then(|_| system.schedule(cpu.as_mut(), 1).map(|_| ()));
                completed.send((cpu_index, result)).unwrap();
            }));
        }
        drop(completed);

        ready.wait();
        let registry = system.state.lock();
        let root_domain = system.root_domain.lock();
        start.wait();
        let mut observed = Vec::new();
        let mut timed_out = false;
        for _ in 0..2 {
            match progress.recv_timeout(Duration::from_secs(2)) {
                Ok(result) => observed.push(result),
                Err(_) => {
                    timed_out = true;
                    break;
                }
            }
        }
        drop(root_domain);
        drop(registry);
        for worker in workers {
            worker.join().unwrap();
        }

        assert!(!timed_out, "owner scheduling waited for a cold lock domain");
        assert_eq!(observed.len(), 2);
        for (_, result) in observed {
            result.unwrap();
        }
    }

    #[test]
    fn current_extension_lookup_progresses_while_registry_is_locked() {
        let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let extension = unsafe { ThreadExtension::new(0x55, &DEADLINE_TEST_EXTENSION_OPS) };
        system
            .install_bootstrap_thread(
                cpu.as_mut(),
                ThreadSpec::new(SchedulePolicy::default()).with_extension(extension),
            )
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());

        let registry = system.state.lock();
        let lease = crate::current_thread_extension().unwrap().unwrap();
        assert_eq!(lease.data(), 0x55);
        assert!(core::ptr::eq(lease.ops(), &DEADLINE_TEST_EXTENSION_OPS));
        drop(registry);
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

        cpu.as_mut().clear_current();
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
    fn context_is_bound_to_the_allocated_thread_before_new_is_published() {
        crate::test_runtime::configure_context_binding(RuntimeStatus::Success);
        let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
        let context = unsafe {
            // SAFETY: the unit runtime models this non-zero scalar as a live
            // context until the task-system fixture is dropped.
            ExecutionContextHandle::from_raw(0x1000)
        };
        let resources = unsafe {
            // SAFETY: the fake runtime accepts the unique context handle above;
            // the remaining resource handles are intentionally absent.
            ThreadResources::new(
                context,
                crate::runtime::StackHandle::NONE,
                crate::runtime::TlsHandle::NONE,
                crate::runtime::AddressSpaceHandle::NONE,
            )
        };

        let thread = system
            .create_thread(unsafe {
                // SAFETY: this specification is the sole owner of `resources`.
                ThreadSpec::new(Default::default()).with_resources(resources)
            })
            .unwrap();

        assert_eq!(system.thread_state(thread.id()), Ok(ThreadState::New));
        assert_eq!(
            crate::test_runtime::last_context_binding(),
            Some(ContextThreadBinding {
                context,
                identity: ThreadIdentityV1::new(thread.id().slot(), thread.id().generation()),
            })
        );
    }

    #[test]
    fn failed_context_binding_retires_the_allocated_generation() {
        crate::test_runtime::configure_context_binding(RuntimeStatus::InvalidHandle);
        let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
        let context = unsafe {
            // SAFETY: the unit runtime validates this modeled handle through its
            // configured failing context-binding result.
            ExecutionContextHandle::from_raw(0x2000)
        };
        let resources = unsafe {
            // SAFETY: ownership is transferred once into the failed create path.
            ThreadResources::new(
                context,
                crate::runtime::StackHandle::NONE,
                crate::runtime::TlsHandle::NONE,
                crate::runtime::AddressSpaceHandle::NONE,
            )
        };

        let error = system
            .create_thread(unsafe {
                // SAFETY: this specification is the sole resource owner.
                ThreadSpec::new(Default::default()).with_resources(resources)
            })
            .unwrap_err();
        assert_eq!(
            error,
            TaskError::RuntimeFailure(RuntimeStatus::InvalidHandle as u32)
        );
        let failed = crate::test_runtime::last_context_binding().unwrap();

        crate::test_runtime::configure_context_binding(RuntimeStatus::Success);
        let replacement = system
            .create_thread(ThreadSpec::new(Default::default()))
            .unwrap();
        assert_eq!(replacement.id().slot(), failed.identity.slot);
        assert_ne!(replacement.id().generation(), failed.identity.generation);
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
    fn failed_runtime_switch_tail_keeps_outgoing_context_unreclaimable() {
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
        system.exit_current(cpu.as_mut()).unwrap();
        crate::test_runtime::configure_context_switch_tail(RuntimeStatus::InvalidHandle);

        assert_eq!(
            system.complete_context_switch(cpu.as_mut()),
            Err(TaskError::RuntimeFailure(
                RuntimeStatus::InvalidHandle as u32
            ))
        );
        assert_eq!(crate::test_runtime::context_switch_tail_count(), 1);
        assert!(cpu.switch_handoff().is_some());
        assert_eq!(system.reap_thread(exiting), Err(TaskError::ThreadBusy));

        crate::test_runtime::configure_context_switch_tail(RuntimeStatus::Success);
        system.complete_context_switch(cpu.as_mut()).unwrap();
        assert_eq!(crate::test_runtime::context_switch_tail_count(), 1);
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
            .sched
            .lock()
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
            .sched
            .lock()
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
            .sched
            .lock()
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
            .sched
            .lock()
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
        let sched = record.sched.lock();
        let transitioned = sched.entity.fair().unwrap();
        assert_eq!(sched.charged_runtime_ns, 1_000_000);
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
            .sched
            .lock()
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
    fn schedule_out_rechecks_affinity_under_the_thread_lock() {
        let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
        let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
        let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
        let running = system
            .install_bootstrap_thread(cpu0.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        let idle0 = system
            .register_idle_thread(
                cpu0.as_mut(),
                ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
            )
            .unwrap();
        system
            .register_idle_thread(
                cpu1.as_mut(),
                ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
            )
            .unwrap();
        system.bring_cpu_online(cpu0.as_mut()).unwrap();
        system.bring_cpu_online(cpu1.as_mut()).unwrap();

        // Model the exact SMP interleaving that used to corrupt CpuLocal:
        // the owner observed no migration, then a remote affinity writer made
        // this CPU illegal before requeue acquired the thread lock. Affinity is
        // authoritative even if a stale migration hint has not been installed.
        let mut target_only = CpuSet::empty(2);
        assert!(target_only.insert(CpuId::new(1)));
        {
            let mut sched = running.core.sched().lock();
            sched.affinity = target_only;
            sched.migration_target = None;
        }
        running.core.set_target_cpu(CpuId::new(1));

        let decision = system.schedule(cpu0.as_mut(), 1).unwrap();
        assert_eq!(decision.switch_reason(), SwitchReason::Migrated);
        assert_eq!(decision.next(), idle0.id());
        assert_eq!(cpu0.current(), Some(idle0.id()));
        assert_eq!(system.thread_state(running.id()), Ok(ThreadState::Ready));
        assert_eq!(
            running.core.sched().lock().migration_target,
            Some(CpuId::new(1))
        );
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
    fn effective_rt_entity_never_replaces_the_base_rr_accounting() {
        let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let base =
            SchedulePolicy::round_robin_with_quantum(RtPriority::new(20).unwrap(), 10).unwrap();
        let owner = system.create_thread(ThreadSpec::new(base)).unwrap();
        let donor = system
            .create_thread(ThreadSpec::new(SchedulePolicy::fifo(
                RtPriority::new(80).unwrap(),
            )))
            .unwrap();
        system.make_ready(owner.id()).unwrap();
        system.enqueue(cpu.as_mut(), owner.id(), 0).unwrap();
        let _wait = system
            .pi_wait_start(PiLockId::new(0x5151), donor.id(), owner.id())
            .unwrap();
        system.drain_policy_updates(cpu.as_mut(), 0).unwrap();

        let state = system.state.lock();
        let sched = state.thread_record(owner.id()).unwrap().sched.lock();
        assert!(
            sched.base_entity.matches_policy(base),
            "PI effective entity must not become base RR accounting"
        );
        assert!(matches!(sched.policy, SchedulePolicy::Fifo { .. }));
        assert!(
            sched.entity.matches_policy(sched.policy),
            "effective policy and entity must be published as one coherent snapshot"
        );
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
        system
            .set_thread_policy(donor.id(), SchedulePolicy::default())
            .unwrap();
        system.drain_policy_updates(cpu.as_mut(), 10).unwrap();
        assert_eq!(system.dispatch_deadline_overruns(1), 1);
        assert_eq!(DEADLINE_OVERRUN_CALLBACKS.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn remote_pi_owner_exclusively_borrows_the_donor_cbs_entity() {
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
        let donor_policy = SchedulePolicy::deadline(
            DeadlinePolicy::new(10, 20, 100, DeadlineFlags::RECLAIM).unwrap(),
        );
        let donor = system.create_thread(ThreadSpec::new(donor_policy)).unwrap();
        let owner = system
            .create_thread(ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        for thread in [&donor, &owner] {
            system.make_ready(thread.id()).unwrap();
        }
        system.enqueue(cpu0.as_mut(), donor.id(), 0).unwrap();
        system.enqueue(cpu1.as_mut(), owner.id(), 0).unwrap();
        assert_eq!(
            system.schedule(cpu0.as_mut(), 0).unwrap().next(),
            donor.id()
        );
        assert_eq!(
            system.schedule(cpu1.as_mut(), 0).unwrap().next(),
            owner.id()
        );

        let _wait = system
            .pi_wait_start(PiLockId::new(0xC85), donor.id(), owner.id())
            .unwrap();
        assert_ne!(
            system.block_current(cpu0.as_mut()).unwrap().next(),
            donor.id()
        );
        system.complete_context_switch(cpu0.as_mut()).unwrap();
        system.drain_policy_updates(cpu1.as_mut(), 0).unwrap();

        let (borrowed_generation, budget_before_timer) = {
            let state = system.state.lock();
            let sched = state.thread_record(donor.id()).unwrap().sched.lock();
            assert_eq!(sched.deadline_cbs_borrower, Some(owner.id()));
            (
                sched.deadline_cbs_generation,
                sched
                    .base_deadline
                    .expect("Deadline donor must retain CBS state"),
            )
        };
        system.service_deadline_timers(cpu0.as_mut(), 20).unwrap();
        {
            let state = system.state.lock();
            let sched = state.thread_record(donor.id()).unwrap().sched.lock();
            assert_eq!(sched.deadline_cbs_borrower, Some(owner.id()));
            assert_eq!(sched.deadline_cbs_generation, borrowed_generation);
            assert_eq!(sched.base_deadline, Some(budget_before_timer));
        }

        cpu1.as_mut()
            .fields_mut()
            .add_deadline_bandwidth(500_000_000, false)
            .unwrap();
        system.charge_current(cpu1.as_mut(), 5, 5, 0).unwrap();
        TaskSystem::commit_owner_current_dispatch(cpu1.as_mut(), 5).unwrap();
        {
            let state = system.state.lock();
            let sched = state.thread_record(donor.id()).unwrap().sched.lock();
            assert_eq!(sched.deadline_cbs_borrower, None);
            assert!(sched.deadline_cbs_generation > borrowed_generation);
            assert_eq!(
                sched
                    .base_deadline
                    .expect("committed donor budget must remain Deadline")
                    .remaining_runtime_ns(),
                5
            );
        }
        system.service_deadline_timers(cpu0.as_mut(), 20).unwrap();
        assert_eq!(system.deadline_runtime(donor.id()).unwrap().misses(), 1);
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
