//! Generation-checked registry and scheduling orchestration.

mod model;
mod outcome;
mod pi;
mod registry;

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
    inbox::{InboxKind, InboxMessage, PublishResult, SchedulerInbox},
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
        let _publication = IrqScope::enter();
        let result = self.deferred_reclaims.publish(
            node.inbox(),
            InboxMessage::reclaim(ThreadId::from_parts(0, 0), 0, data),
        );
        if result != PublishResult::WrongKind {
            self.task_work.publish();
        }
        result
    }

    pub(crate) fn task_work_doorbell(&self) -> Arc<TaskWorkDoorbell> {
        Arc::clone(&self.task_work)
    }

    pub(crate) fn begin_task_work_worker_install(&self) -> Result<(), TaskError> {
        self.task_work.begin_worker_install()
    }

    pub(crate) fn finish_task_work_worker_install(&self) {
        self.task_work.finish_worker_install();
    }

    pub(crate) fn cancel_task_work_worker_install(&self) {
        self.task_work.cancel_worker_install();
    }

    /// Reports whether a sticky task-work publication awaits the service thread.
    pub fn deferred_task_work_pending(&self) -> bool {
        self.task_work.is_pending()
    }

    /// Runs one bounded task-context pass as the single task-work consumer.
    ///
    /// Unrelated work classes are interleaved through a persistent round-robin
    /// cursor. Per-thread claim predicates still enforce Deadline callback,
    /// exit callback, and record-reaping order. A concurrent or reentrant
    /// consumer receives [`TaskError::ThreadBusy`] without consuming work.
    pub fn service_deferred_task_work(
        &self,
        limit: usize,
    ) -> Result<DeferredTaskWorkBatch, TaskError> {
        if task_runtime::in_hard_irq() {
            return Err(TaskError::UnsafeContext);
        }
        let limit = limit.min(crate::DEFAULT_BATCH_LIMIT);
        if limit == 0 {
            return Ok(DeferredTaskWorkBatch::default());
        }
        let _consumer: TaskWorkConsumerGuard<'_> = self.task_work.try_claim_consumer()?;
        let mut next_class = self.state.lock().task_work_class_cursor;
        let outcome = (|| {
            let mut batch = DeferredTaskWorkBatch::default();
            let mut classes_without_progress = 0;
            while batch.processed() < limit
                && classes_without_progress < DeferredTaskWorkClass::COUNT
            {
                let class = next_class;
                next_class = class.next();
                let processed = match class {
                    DeferredTaskWorkClass::Deadline => {
                        let (events, callbacks) = self.dispatch_deadline_overruns_inner(1)?;
                        batch.deadline_events += events;
                        batch.deadline_callbacks += callbacks;
                        events
                    }
                    DeferredTaskWorkClass::Exit => {
                        let callbacks = self.dispatch_exit_callbacks_inner(1)?;
                        batch.exit_callbacks += callbacks;
                        callbacks
                    }
                    DeferredTaskWorkClass::Reap => {
                        let reaped = self.reap_unreferenced_exited_inner(1)?;
                        batch.reaped_threads += reaped;
                        reaped
                    }
                    DeferredTaskWorkClass::Reclaim => {
                        let reclaimed = self.reclaim_one_resource()?;
                        batch.reclaimed_resources += reclaimed;
                        reclaimed
                    }
                };
                if processed == 0 {
                    classes_without_progress += 1;
                } else {
                    classes_without_progress = 0;
                }
            }
            debug_assert!(batch.processed() <= limit);
            Ok(batch)
        })();
        self.state.lock().task_work_class_cursor = next_class;
        outcome
    }

    fn reclaim_one_resource(&self) -> Result<usize, TaskError> {
        let thread_release_first = {
            let mut state = self.state.lock();
            let current = state.thread_release_first;
            state.thread_release_first = !current;
            current
        };
        if thread_release_first {
            if self.retry_pending_resource_release() {
                return Ok(1);
            }
            self.drain_deferred_reclaims_inner(1)
        } else {
            let reclaimed = self.drain_deferred_reclaims_inner(1)?;
            if reclaimed != 0 {
                Ok(reclaimed)
            } else {
                Ok(usize::from(self.retry_pending_resource_release()))
            }
        }
    }

    fn retry_pending_resource_release(&self) -> bool {
        let Some(mut pending) = self.state.lock().pending_resource_releases.pop() else {
            return false;
        };
        match pending.resources_mut().try_release() {
            Ok(()) => {
                pending.finish();
            }
            Err(_error) => {
                self.state.lock().pending_resource_releases.push(pending);
                // A failed release retains every live handle. Re-publish after
                // returning it to the queue so the service thread yields
                // between retries instead of spinning inside one batch.
                self.task_work.publish();
            }
        }
        true
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
        if task_runtime::in_hard_irq() {
            return Err(TaskError::UnsafeContext);
        }
        let _consumer = self.task_work.try_claim_consumer()?;
        self.drain_deferred_reclaims_inner(limit)
    }

    fn drain_deferred_reclaims_inner(&self, limit: usize) -> Result<usize, TaskError> {
        const MAX_DRAIN_BATCH: usize = 64;

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

    /// Returns cumulative non-idle runtime charged by one online CPU.
    pub fn cpu_busy_runtime_ns(&self, cpu: CpuId) -> Result<u64, TaskError> {
        let remote = self
            .cpu_remotes
            .get(cpu.as_usize())
            .ok_or(TaskError::InvalidCpu(cpu.as_u32()))?;
        if !remote.is_online() {
            return Err(TaskError::CpuOffline(cpu.as_u32()));
        }
        Ok(remote.busy_runtime_ns())
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
        self.bring_cpu_online_at(cpu, task_runtime::monotonic_ns())
    }

    /// Completes CPU registration at `now_ns` and publishes it online.
    ///
    /// The explicit clock sample keeps deterministic scheduler models and OS
    /// runtimes on the same absolute monotonic time base. In particular, the
    /// first fair-balance deadline is one interval after online publication,
    /// rather than one interval after an unrelated zero epoch.
    pub fn bring_cpu_online_at(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<(), TaskError> {
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
        cpu.as_mut()
            .reset_fair_balance(now_ns, self.config.balance_interval_ns());
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
        let affinity = spec
            .affinity()
            .cloned()
            .unwrap_or_else(|| CpuSet::all(self.config.cpu_count()));
        let unpublished = UnpublishedThreadGuard::new(self, spec);
        policy.validate()?;
        validate_affinity(&affinity, self.config.cpu_count())?;
        let mut state = self.state.lock();
        self.drain_pending_deadline_admission(&mut state);
        let root_domain = self.root_domain.lock();
        let reservation = state.reserve_deadline(policy, &affinity, &root_domain.online)?;
        drop(root_domain);
        let (slot, generation) = match state.allocate_thread_slot() {
            Ok(identity) => identity,
            Err(error) => {
                state.deadline_admission.release(reservation);
                return Err(error);
            }
        };
        let id = ThreadId::from_parts(slot, generation);
        let entity = SchedulingEntity::new(policy, self.config.fair_slice_ns(), 0);
        let base_deadline = match entity {
            SchedulingEntity::Deadline(deadline) => Some(deadline),
            _ => None,
        };
        let (extension, resources) = unpublished.into_owned_parts();
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
                placement: SchedulerPlacement::detached(),
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
            Some(Arc::clone(&self.task_work)),
        ));
        let record = ThreadRecord {
            core: Arc::clone(&core),
            sched,
            resources,
            extension,
            blocked_on: None,
            exit_callback_pending: false,
            exit_callback_claimed: false,
            deadline_callback_claimed: false,
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
                if advance_thread_slot_generation(failed_slot) {
                    state.free_slots.push(slot);
                }
                state.deadline_admission.release(reservation);
                drop(state);
                drop(core);
                let _rollback = self.release_thread_record(record);
                return Err(TaskError::RuntimeFailure(status as u32));
            }
        }
        state.slots[slot as usize].record = Some(record);
        Ok(ThreadHandle::from_core(core))
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
        {
            let state = self.state.lock();
            let registration = state.cpu_registration(cpu.owner())?;
            if !Arc::ptr_eq(&registration.remote, cpu.remote()) {
                return Err(TaskError::InvalidRuntimeHandle);
            }
            if cpu.current().is_some() {
                return Err(TaskError::InvalidConfiguration);
            }
        }

        let thread = self.create_thread(spec)?;
        let setup = (|| {
            let state = self.state.lock();
            let record = state.thread_record(thread.id())?;
            let core = Arc::clone(&record.core);
            let dispatch = {
                let mut sched = record.sched.lock();
                sched.transition(&core, ThreadState::Ready)?;
                sched.transition(&core, ThreadState::Running)?;
                let dispatch = Self::owner_dispatch(&core, &sched, task_runtime::monotonic_ns())?;
                sched.placement.set_running_cpu(Some(cpu.owner()))?;
                sched.placement.set_on_cpu(Some(cpu.owner()))?;
                core.set_target_cpu(cpu.owner());
                dispatch
            };
            cpu.as_mut().set_current_core(Arc::clone(&core));
            cpu.as_mut().install_dispatch(dispatch);
            drop(state);
            self.publish_owner_cpu_load_summary(cpu.as_mut());
            Ok(())
        })();
        if let Err(error) = setup {
            return match self.discard_unpublished_thread(thread) {
                Ok(()) => Err(error),
                Err(cleanup_error) => Err(cleanup_error),
            };
        }
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
        {
            let state = self.state.lock();
            let registration = state.cpu_registration(cpu.owner())?;
            if !Arc::ptr_eq(&registration.remote, cpu.remote()) {
                return Err(TaskError::InvalidRuntimeHandle);
            }
            if cpu.idle().is_some() {
                return Err(TaskError::InvalidConfiguration);
            }
        }

        let thread = self.create_thread(spec)?;
        let setup = self.make_ready(thread.id()).and_then(|()| {
            let state = self.state.lock();
            let core = Arc::clone(&state.thread_record(thread.id())?.core);
            cpu.as_mut().set_idle(thread.id(), core);
            Ok(())
        });
        if let Err(error) = setup {
            return match self.discard_unpublished_thread(thread) {
                Ok(()) => Err(error),
                Err(cleanup_error) => Err(cleanup_error),
            };
        }
        Ok(thread)
    }

    fn discard_unpublished_thread(&self, handle: ThreadHandle) -> Result<(), TaskError> {
        let record = self
            .state
            .lock()
            .remove_unpublished_thread_with_handle(&handle)?;
        drop(handle);
        self.release_thread_record(record)
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
        let mut detached = [InboxMessage::EMPTY; crate::DEFAULT_BATCH_LIMIT];
        detached[..drained].copy_from_slice(&cpu.migration_buffer[..drained]);
        let mut messages = DetachedOwnerMessageBatch::new(
            &detached[..drained],
            DetachedPayloadKind::SchedulerDelivery,
        );
        while let Some(message) = messages.next() {
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
            if source == target {
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
            if source != target {
                if target == owner {
                    let latest_target = core.sched().lock().placement.migration_target();
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
                            || sched.placement.queued_cpu().is_some()
                            || sched.placement.running_cpu().is_some()
                            || sched.placement.on_cpu().is_some()
                        {
                            return Err(TaskError::InvalidConfiguration);
                        }
                        sched.placement.set_migration_target(None)?;
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
                            sched.placement.queued_cpu(),
                            sched.placement.running_cpu(),
                            sched.lifecycle.state(),
                            sched.placement.migration_target(),
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
                            sched.placement.set_queued_cpu(None)?;
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
                        core.sched().lock().placement.set_migration_target(None)?;
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
        Ok(self.finish_owner_selection(cpu, decision, now_ns))
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
        Ok(self.finish_owner_selection(cpu, decision, now_ns))
    }

    /// Publishes `PARKING` after consuming a wake-before-park notification.
    pub fn prepare_park(&self, mut cpu: Pin<&mut CpuLocal>) -> Result<ParkPrepare, TaskError> {
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
        let next_core = self.pick_owner_next(cpu.as_mut(), now_ns, Some(token.thread()))?;
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
        Ok(self.finish_owner_selection(cpu, decision, now_ns))
    }

    /// Completes the physical switch-out handoff in the newly active context.
    ///
    /// This second phase clears `on_cpu` only after architecture execution has
    /// left the previous stack. Deferred migration publication and exit hooks
    /// therefore cannot make a context runnable or reapable too early.
    pub fn complete_context_switch(&self, mut cpu: Pin<&mut CpuLocal>) -> Result<(), TaskError> {
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
        let (source, remains_placed) = {
            sched.affinity = affinity;
            let location = sched
                .placement
                .running_cpu()
                .or(sched.placement.queued_cpu());
            let source = match location {
                Some(owner) if !sched.affinity.contains(owner) => {
                    sched.placement.set_migration_target(Some(target))?;
                    Some(owner)
                }
                Some(owner) => {
                    // A newer mask made the owner legal again before its
                    // pending migration request ran. Cancel that request.
                    sched.placement.set_migration_target(None)?;
                    core.set_target_cpu(owner);
                    None
                }
                None if sched.placement.migration_target().is_some() => {
                    // The source already detached this ready thread and a
                    // transfer is in flight. Retarget the transfer in-place;
                    // the old destination forwards it after observing this
                    // state under the scheduler lock.
                    sched.placement.set_migration_target(Some(target))?;
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
        let state = self.state.lock();
        state.cpu_registration(cpu.owner())?;
        let core = Arc::clone(&state.thread_record(thread)?.core);
        cpu.as_mut().set_idle(thread, core);
        Ok(())
    }

    /// Marks a non-queued thread exited and queues its task-context exit hook.
    pub fn mark_exited(&self, thread: ThreadId) -> Result<(), TaskError> {
        {
            let mut state = self.state.lock();
            let cleanup_deadline_member = {
                let record = state.thread_record_mut(thread)?;
                let mut sched = record.sched.lock();
                if sched.placement.queued_cpu().is_some() || sched.placement.running_cpu().is_some()
                {
                    return Err(TaskError::AlreadyQueued);
                }
                if sched.placement.on_cpu().is_some() {
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
            {
                let record = state.thread_record_mut(thread)?;
                let _exit = record
                    .core
                    .try_scheduler_exit()
                    .ok_or(TaskError::ThreadBusy)?;
                let mut sched = record.sched.lock();
                if sched.placement.queued_cpu().is_some() || sched.placement.running_cpu().is_some()
                {
                    return Err(TaskError::AlreadyQueued);
                }
                if sched.placement.on_cpu().is_some() || sched.deadline_cbs_borrower.is_some() {
                    return Err(TaskError::ThreadBusy);
                }
                if record.blocked_on.is_some() || sched.blocked_pi_waiters != 0 {
                    return Err(TaskError::InvalidPiState);
                }
                sched.placement.set_migration_target(None)?;
                sched.transition(&record.core, ThreadState::Exited)?;
                record.exit_callback_pending = record.extension.is_some();
                record.exit_callback_claimed = false;
            }
            state.release_deadline_reservation_on_exit(thread)?;
        }
        self.task_work.publish();
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
        let _consumer = self.task_work.try_claim_consumer()?;
        self.dispatch_exit_callbacks_inner(limit)
    }

    fn dispatch_exit_callbacks_inner(&self, limit: usize) -> Result<usize, TaskError> {
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
        if task_runtime::in_hard_irq() {
            return Err(TaskError::UnsafeContext);
        }
        let record = {
            let mut state = self.state.lock();
            state.remove_exited_thread(thread)?
        };
        self.release_thread_record(record)
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
        self.release_thread_record(record)
            .map_err(OwnedThreadReapError::Committed)
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
        let _consumer = self.task_work.try_claim_consumer()?;
        self.reap_unreferenced_exited_inner(limit)
    }

    fn reap_unreferenced_exited_inner(&self, limit: usize) -> Result<usize, TaskError> {
        let mut reaped = 0;
        while reaped < limit {
            let record = {
                let mut state = self.state.lock();
                state.take_unreferenced_exited()?
            };
            let Some(record) = record else {
                break;
            };
            // Registry removal is already committed. A failed runtime release
            // moves the complete record into the task-work retry queue instead
            // of losing its resource handles or terminating the service loop.
            let _release = self.release_thread_record(record);
            reaped += 1;
        }
        Ok(reaped)
    }

    fn release_thread_record(&self, mut record: ThreadRecord) -> Result<(), TaskError> {
        match record.resources.try_release() {
            Ok(()) => {
                drop(record.extension.take());
                Ok(())
            }
            Err(error) => {
                self.state
                    .lock()
                    .pending_resource_releases
                    .push(PendingResourceRelease::Thread(record));
                self.task_work.publish();
                Err(error)
            }
        }
    }

    fn release_unpublished_thread(
        &self,
        mut record: DetachedThreadRecord,
    ) -> Result<(), TaskError> {
        match record.try_release_resources() {
            Ok(()) => {
                record.finish_release();
                Ok(())
            }
            Err(error) => {
                self.state
                    .lock()
                    .pending_resource_releases
                    .push(PendingResourceRelease::Detached(record));
                self.task_work.publish();
                Err(error)
            }
        }
    }

    /// Releases a construction transaction that failed before thread registry
    /// publication.
    ///
    /// If the runtime reports a retryable failure, this method retains the
    /// complete bundle and publishes task-context reaper work. The returned
    /// error describes the first failed release operation; ownership never
    /// returns to the caller.
    pub fn release_unpublished_resources(
        &self,
        resources: ThreadResources,
    ) -> Result<(), TaskError> {
        self.release_unpublished_thread(DetachedThreadRecord::new(resources, None))
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
            || sched.placement.running_cpu() != Some(owner)
            || sched.placement.on_cpu() != Some(owner)
            || sched.placement.queued_cpu().is_some()
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
        Ok(ThreadHandle::from_core(Arc::clone(&record.core)))
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
        if sched.lifecycle.state() == ThreadState::Exited {
            return Err(TaskError::NotReady);
        }
        let active_reservation = u128::from(sched.active_deadline_reservation);
        let desired_reservation = u128::from(sched.desired_deadline_reservation);
        let affinity = sched.affinity.clone();
        let owner = sched
            .placement
            .running_cpu()
            .or(sched.placement.queued_cpu())
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
                sched.placement.set_queued_cpu(None)?;
                sched.placement.set_running_cpu(Some(owner))?;
                sched.placement.set_on_cpu(Some(owner))?;
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
                sched.placement.set_running_cpu(Some(owner))?;
                sched.placement.set_on_cpu(Some(owner))?;
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
