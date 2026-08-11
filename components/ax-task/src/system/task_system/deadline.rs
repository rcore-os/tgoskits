//! Deadline diagnostics, deferred callbacks, and owner timer service.

use super::*;
use crate::{
    SchedulerClockEvent, runtime::SchedulerDeadlineUpdate, scheduler_clock_event,
    scheduler_time_reached,
};

enum OwnerDeadlineTimerPlan {
    Unchanged,
    Cancel,
    Arm(TaskDeadlineArmPlan),
}

#[derive(Clone, Copy)]
struct OwnerDeadlineDue {
    scheduler_now_ns: u64,
    cbs_expired: bool,
    zero_lag_reached: bool,
}

struct OwnerDeadlineReconcile<'a> {
    core: &'a Arc<ThreadCore>,
    sched: &'a mut ThreadSchedState,
    cpu: Pin<&'a mut CpuLocal>,
    due: OwnerDeadlineDue,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct KtimerServiceBatch {
    processed: usize,
    pending: bool,
    update: Option<SchedulerDeadlineUpdate>,
}

impl KtimerServiceBatch {
    #[cfg(test)]
    pub(crate) const fn processed(self) -> usize {
        self.processed
    }

    pub(crate) const fn pending(self) -> bool {
        self.pending
    }

    pub(crate) const fn update(self) -> Option<SchedulerDeadlineUpdate> {
        self.update
    }
}

impl TaskSystem {
    pub(super) fn publish_deadline_overrun_work(&self, core: Arc<ThreadCore>) {
        let core_ptr = Arc::into_raw(core);
        let node = unsafe {
            // SAFETY: Arc allocations remain pinned, and the strong count
            // transferred below keeps the embedded node alive until drain.
            Pin::new_unchecked((&*core_ptr).deadline_callback_node())
        };
        let result = self.deferred_deadline_callbacks.publish(
            node,
            InboxMessage::deadline_overrun(
                unsafe {
                    // SAFETY: the transferred Arc keeps this core alive.
                    (*core_ptr).id()
                },
                core_ptr.expose_provenance(),
            ),
        );
        if result == PublishResult::Published {
            self.task_work.publish();
            return;
        }
        unsafe {
            // SAFETY: a coalesced or rejected publication did not consume the
            // transferred strong count.
            drop(Arc::from_raw(core_ptr));
        }
        if result == PublishResult::WrongKind {
            task_runtime::fatal_invariant(0x444c_0001, result as usize);
        }
    }

    fn task_deadline_error(error: TaskDeadlineError) -> TaskError {
        match error {
            TaskDeadlineError::Capacity => TaskError::TimerCapacity,
            TaskDeadlineError::GenerationExhausted | TaskDeadlineError::KindMismatch => {
                TaskError::InvalidConfiguration
            }
        }
    }

    fn prepare_owner_deadline_timer(
        queue: &TaskDeadlineQueue,
        node: &TaskDeadlineNode,
        registration: Option<&TaskDeadlineRegistration>,
        deadline: Option<MonotonicDeadline>,
        kind: TaskDeadlineKind,
    ) -> Result<OwnerDeadlineTimerPlan, TaskError> {
        if registration.is_some_and(|registration| {
            Some(registration.deadline()) == deadline && registration.kind() == kind
        }) {
            return Ok(OwnerDeadlineTimerPlan::Unchanged);
        }
        let Some(deadline) = deadline else {
            return Ok(if registration.is_some() {
                OwnerDeadlineTimerPlan::Cancel
            } else {
                OwnerDeadlineTimerPlan::Unchanged
            });
        };
        queue
            .prepare_arm(node, deadline, kind)
            .map(OwnerDeadlineTimerPlan::Arm)
            .map_err(Self::task_deadline_error)
    }

    fn commit_owner_deadline_timer(
        queue: &mut TaskDeadlineQueue,
        registration: &mut Option<TaskDeadlineRegistration>,
        plan: OwnerDeadlineTimerPlan,
    ) {
        match plan {
            OwnerDeadlineTimerPlan::Unchanged => {}
            OwnerDeadlineTimerPlan::Cancel => {
                if let Some(previous) = registration.take() {
                    // Expiration may already have moved the entry into the
                    // safe-point buffer. The registration is terminal either
                    // way; a later token makes the buffered copy stale.
                    let _removed = queue.cancel(&previous);
                }
            }
            OwnerDeadlineTimerPlan::Arm(plan) => {
                *registration = Some(queue.commit_arm(plan));
            }
        }
    }

    pub(super) fn refresh_owner_deadline_timers_locked(
        &self,
        core: &Arc<ThreadCore>,
        sched: &mut ThreadSchedState,
        mut cpu: Pin<&mut CpuLocal>,
    ) {
        let remote = Arc::clone(cpu.remote());
        let mut transaction = OwnerRqTxn::begin(self, &remote);
        let scheduler_now_ns = transaction.clock().wall().as_nanos();
        let enqueue = self.refresh_owner_deadline_timers_in_rq(
            core,
            sched,
            cpu.as_mut(),
            scheduler_now_ns,
            &mut transaction,
        );
        transaction.commit();
        if let Some(preempts_current) = enqueue {
            self.finish_owner_enqueue(cpu, EnqueueReason::Replenished, preempts_current);
        }
    }

    pub(super) fn refresh_owner_deadline_timers_in_rq(
        &self,
        core: &Arc<ThreadCore>,
        sched: &mut ThreadSchedState,
        mut cpu: Pin<&mut CpuLocal>,
        scheduler_now_ns: u64,
        run_queue: &mut OwnerRqTxn<'_>,
    ) -> Option<bool> {
        let monotonic_now = task_runtime::monotonic_now();
        let mut owner_enqueue = None;

        loop {
            let owner = cpu.owner();
            let owns_bandwidth = sched.deadline.bandwidth.reservation_owner() == Some(owner);
            let owner_entity = if let Some(active) = sched.policy.active_option() {
                active.base_entity().clone()
            } else {
                run_queue
                    .base_scheduling_entity(core.id())
                    .unwrap_or_else(|| {
                        task_runtime::fatal_invariant(0x444c_1101, core.id().as_u64() as usize)
                    })
            };
            let cbs_scheduler_deadline = owns_bandwidth
                .then_some(())
                .and(owner_entity.deadline())
                .and_then(DeadlineEntity::next_scheduler_event_ns);
            let zero_lag_scheduler_deadline = owns_bandwidth
                .then(|| sched.deadline.bandwidth.zero_lag())
                .flatten()
                .map(SchedulerTimestamp::as_nanos);
            let cbs_event = cbs_scheduler_deadline
                .map(|deadline| scheduler_clock_event(scheduler_now_ns, monotonic_now, deadline));
            let zero_lag_event = zero_lag_scheduler_deadline
                .map(|deadline| scheduler_clock_event(scheduler_now_ns, monotonic_now, deadline));
            let cbs_due = matches!(cbs_event, Some(SchedulerClockEvent::Due));
            let zero_lag_due = matches!(zero_lag_event, Some(SchedulerClockEvent::Due));
            let cbs_deadline = match cbs_event {
                Some(SchedulerClockEvent::Future(deadline)) => Some(deadline),
                Some(SchedulerClockEvent::Due) | None => None,
            };
            let zero_lag_deadline = match zero_lag_event {
                Some(SchedulerClockEvent::Future(deadline)) => Some(deadline),
                Some(SchedulerClockEvent::Due) | None => None,
            };
            {
                let mut deadline_base = cpu
                    .remote()
                    .lock_deadline_base(DeadlineBaseGuardSource::Registration);
                let cbs_plan = Self::prepare_owner_deadline_timer(
                    &deadline_base.queue,
                    core.deadline_cbs_timer(),
                    sched.deadline.cbs_timer.as_ref(),
                    cbs_deadline,
                    TaskDeadlineKind::DeadlineCbs,
                )
                .unwrap_or_else(|_| {
                    task_runtime::fatal_invariant(0x444c_0006, core.id().as_u64() as usize)
                });
                let zero_lag_plan = Self::prepare_owner_deadline_timer(
                    &deadline_base.queue,
                    core.deadline_zero_lag_timer(),
                    sched.deadline.zero_lag_timer.as_ref(),
                    zero_lag_deadline,
                    TaskDeadlineKind::DeadlineZeroLag,
                )
                .unwrap_or_else(|_| {
                    task_runtime::fatal_invariant(0x444c_0007, core.id().as_u64() as usize)
                });
                Self::commit_owner_deadline_timer(
                    &mut deadline_base.queue,
                    &mut sched.deadline.cbs_timer,
                    cbs_plan,
                );
                Self::commit_owner_deadline_timer(
                    &mut deadline_base.queue,
                    &mut sched.deadline.zero_lag_timer,
                    zero_lag_plan,
                );
            }

            if !cbs_due && !zero_lag_due {
                return owner_enqueue;
            }
            let reconcile = OwnerDeadlineReconcile {
                core,
                sched,
                cpu: cpu.as_mut(),
                due: OwnerDeadlineDue {
                    scheduler_now_ns,
                    cbs_expired: cbs_due,
                    zero_lag_reached: zero_lag_due,
                },
            };
            if let Some(preempts_current) =
                self.reconcile_due_owner_deadline_locked(reconcile, run_queue)
            {
                owner_enqueue = Some(owner_enqueue.unwrap_or(false) || preempts_current);
            }
        }
    }

    fn reconcile_due_owner_deadline_locked(
        &self,
        reconcile: OwnerDeadlineReconcile<'_>,
        run_queue: &mut OwnerRqTxn<'_>,
    ) -> Option<bool> {
        let OwnerDeadlineReconcile {
            core,
            sched,
            cpu,
            due,
        } = reconcile;
        let mut replenish = false;

        if due.cbs_expired {
            let rq_throttled = run_queue.is_deadline_throttled_member(core.id());
            let base_entity = if let Some(active) = sched.policy.active_option() {
                active.base_entity().clone()
            } else {
                run_queue
                    .base_scheduling_entity(core.id())
                    .unwrap_or_else(|| {
                        task_runtime::fatal_invariant(0x444c_1102, core.id().as_u64() as usize)
                    })
            };
            let Some(deadline) = base_entity.deadline() else {
                task_runtime::fatal_invariant(0x444c_1103, core.id().as_u64() as usize);
            };
            let replenish_due = deadline.is_throttled()
                && deadline
                    .next_scheduler_event_ns()
                    .is_some_and(|event| scheduler_time_reached(due.scheduler_now_ns, event));
            if replenish_due {
                deadline.replenish(due.scheduler_now_ns);
                let updated = SchedulingEntity::Deadline(deadline.clone());
                if let Some(active) = sched.policy.active_option() {
                    let _ = active;
                    sched.policy.active_mut().replace_base_entity(updated);
                } else {
                    let updated_in_rq = if rq_throttled && !deadline.is_throttled() {
                        replenish = true;
                        run_queue
                            .replenish_throttled_deadline(core.id(), updated.clone())
                            .is_ok()
                    } else {
                        run_queue.update_base_deadline_entity(core.id(), updated.clone())
                    };
                    if !updated_in_rq {
                        task_runtime::fatal_invariant(0x444c_1104, core.id().as_u64() as usize);
                    }
                    if core.effective_policy_snapshot() == sched.policy.base {
                        core.publish_effective_schedule(sched.policy.base, &updated);
                    }
                }
            }
        }

        if due.zero_lag_reached
            && sched.deadline.bandwidth.zero_lag().is_some_and(|zero_lag| {
                zero_lag.is_reached_by(SchedulerTimestamp::from_nanos(due.scheduler_now_ns))
            })
        {
            run_queue.deactivate_deadline_bandwidth(sched.deadline.bandwidth.reservation_scaled());
            sched.deadline.bandwidth.deactivate();
        }

        if replenish {
            if sched.lifecycle.state() != ThreadState::Ready
                || sched.placement.queued_cpu() != Some(cpu.owner())
                || sched.placement.on_cpu().is_some()
            {
                task_runtime::fatal_invariant(0x444c_1108, core.id().as_u64() as usize);
            }
            let entity = run_queue.scheduling_entity(core.id()).unwrap_or_else(|| {
                task_runtime::fatal_invariant(0x444c_1105, core.id().as_u64() as usize)
            });
            let preempts_current = run_queue
                .wakeup_preempt(core.id(), sched.policy.base, entity, 0)
                .requests_reschedule();
            core.set_wake_cpu_hint(cpu.owner());
            return Some(preempts_current);
        }
        None
    }

    pub(super) fn cancel_owner_deadline_timers_locked(
        core: &Arc<ThreadCore>,
        sched: &mut ThreadSchedState,
        remote: &CpuRemote,
    ) {
        let mut deadline_base = remote.lock_deadline_base(DeadlineBaseGuardSource::Registration);
        let cbs_plan = Self::prepare_owner_deadline_timer(
            &deadline_base.queue,
            core.deadline_cbs_timer(),
            sched.deadline.cbs_timer.as_ref(),
            None,
            TaskDeadlineKind::DeadlineCbs,
        )
        .unwrap_or_else(|_| {
            task_runtime::fatal_invariant(0x444c_0008, core.id().as_u64() as usize)
        });
        let zero_lag_plan = Self::prepare_owner_deadline_timer(
            &deadline_base.queue,
            core.deadline_zero_lag_timer(),
            sched.deadline.zero_lag_timer.as_ref(),
            None,
            TaskDeadlineKind::DeadlineZeroLag,
        )
        .unwrap_or_else(|_| {
            task_runtime::fatal_invariant(0x444c_0009, core.id().as_u64() as usize)
        });
        Self::commit_owner_deadline_timer(
            &mut deadline_base.queue,
            &mut sched.deadline.cbs_timer,
            cbs_plan,
        );
        Self::commit_owner_deadline_timer(
            &mut deadline_base.queue,
            &mut sched.deadline.zero_lag_timer,
            zero_lag_plan,
        );
    }

    fn registration_matches(
        registration: &TaskDeadlineRegistration,
        event: ExpiredTaskDeadline,
    ) -> bool {
        event.thread() == Some(registration.thread())
            && event.token() == registration.token()
            && event.deadline() == Some(registration.deadline())
            && event.kind() == Some(registration.kind())
    }

    fn take_expired_registration(
        registration: &mut Option<TaskDeadlineRegistration>,
        event: ExpiredTaskDeadline,
    ) -> bool {
        if registration
            .as_ref()
            .is_some_and(|registration| Self::registration_matches(registration, event))
        {
            registration.take();
            true
        } else {
            false
        }
    }

    /// Returns Deadline budget and PI rescue state for diagnostics and ABI glue.
    pub fn deadline_runtime(&self, thread: ThreadId) -> Result<DeadlineRuntimeSnapshot, TaskError> {
        let core = {
            let state = self.state.lock();
            Arc::clone(&state.thread_record(thread)?.core)
        };
        let sched = core.sched().lock();
        let pi_boosted = sched.pi.deadline_donor.is_some();
        let donor = sched.pi.deadline_donor;
        let local_entity = sched.policy.active_option().map(|active| {
            if pi_boosted {
                active.entity().clone()
            } else {
                active.base_entity().clone()
            }
        });
        let effective_entity = if let Some(entity) = local_entity {
            entity
        } else {
            let owner = sched
                .placement
                .assigned_cpu()
                .ok_or(TaskError::InvalidConfiguration)?;
            let remote = self
                .cpu_remotes
                .get(owner.as_usize())
                .ok_or(TaskError::InvalidConfiguration)?;
            // Keep the task-control lock across the owner-rq observation. This
            // is the read-side equivalent of Linux `task_rq_lock()`: policy,
            // placement, and CBS state come from one ordered transaction.
            let transaction = OwnerRqTxn::begin(self, remote);
            let entity = if pi_boosted {
                transaction.scheduling_entity(thread)
            } else {
                transaction.base_scheduling_entity(thread)
            };
            let Some(entity) = entity else {
                transaction.commit();
                return Err(TaskError::InvalidConfiguration);
            };
            transaction.commit();
            entity
        };
        let deadline = effective_entity
            .deadline()
            .ok_or(TaskError::InvalidConfiguration)?;
        Ok(DeadlineRuntimeSnapshot {
            remaining_runtime_ns: deadline.remaining_runtime_ns(),
            overruns: deadline.overruns(),
            pi_boosted,
            donor,
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
        if !matches!(sched.policy.base, SchedulePolicy::Deadline(_)) {
            return Err(TaskError::InvalidConfiguration);
        }
        Ok(DeadlineActivitySnapshot {
            activity: sched.deadline.bandwidth.activity(),
            bandwidth_cpu: sched.deadline.bandwidth.reservation_owner(),
            zero_lag_ns: sched
                .deadline
                .bandwidth
                .zero_lag()
                .map(SchedulerTimestamp::as_nanos),
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

    pub(super) fn dispatch_deadline_overruns_inner(
        &self,
        limit: usize,
    ) -> Result<(usize, usize), TaskError> {
        const MAX_DISPATCH_BATCH: usize = 64;

        let mut messages = [InboxMessage::EMPTY; MAX_DISPATCH_BATCH];
        let batch = self
            .deferred_deadline_callbacks
            .drain(limit.min(MAX_DISPATCH_BATCH), &mut messages);
        let mut dispatched = 0;
        for message in messages.iter().take(batch.drained()) {
            if message.operation() != InboxOperation::DeadlineOverrun || message.payload() == 0 {
                task_runtime::fatal_invariant(0x444c_0002, message.payload());
            }
            let core = unsafe {
                // SAFETY: publication transferred exactly one Arc strong count
                // whose pointer is carried by this detached message.
                Arc::from_raw(ptr::with_exposed_provenance::<ThreadCore>(
                    message.payload(),
                ))
            };
            if core.id() != message.thread_id() {
                task_runtime::fatal_invariant(0x444c_0003, message.payload());
            }
            let claim = self
                .state
                .lock()
                .claim_pending_deadline_overrun(core.id())?;
            match claim {
                DeadlineCallbackClaim::NoCallback { has_more } => {
                    if has_more {
                        self.publish_deadline_overrun_work(Arc::clone(&core));
                    }
                }
                DeadlineCallbackClaim::Callback { extension, thread } => {
                    // SAFETY: the registry's callback claim prevents reaping
                    // while the callback runs, and every scheduler lock was
                    // released above.
                    unsafe {
                        (extension.ops().on_deadline_overrun)(extension.data(), thread);
                    }
                    if self.state.lock().finish_deadline_callback(thread)? {
                        self.publish_deadline_overrun_work(Arc::clone(&core));
                    }
                    dispatched += 1;
                }
            }
        }
        if batch.pending() {
            self.task_work.publish();
        }
        Ok((batch.drained(), dispatched))
    }

    /// Runs one PREEMPT_RT `ktimers/%u` task-context pass.
    ///
    /// Hard IRQ has already moved a bounded set of soft expirations into the
    /// per-CPU deadline base. This pass may promote more due soft entries, wake
    /// their generation-bearing park owners, and then publish the one next
    /// physical deadline selected from the authoritative base.
    pub(crate) fn service_ktimer_work(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
    ) -> Result<KtimerServiceBatch, TaskError> {
        if task_runtime::in_hard_irq() {
            return Err(TaskError::UnsafeContext);
        }
        let monotonic_now = task_runtime::monotonic_now();
        let budget = cpu.batch_limit();
        let mut processed = 0;
        while processed < budget {
            if !cpu.has_expired_task_deadlines() && cpu.has_due_task_deadline(monotonic_now) {
                let remaining = budget - processed;
                cpu.as_mut()
                    .promote_due_task_deadlines(monotonic_now, remaining);
            }
            let Some(event) = cpu
                .remote()
                .lock_deadline_base(DeadlineBaseGuardSource::SoftExpiry)
                .peek_buffered_expiration()
            else {
                break;
            };
            let claimed = match self.service_buffered_expired_deadline(cpu.as_mut(), event) {
                Ok(claimed) => claimed,
                Err(error) => {
                    cpu.remote().publish_ktimer_work();
                    return Err(error);
                }
            };
            if !claimed {
                continue;
            }
            processed += 1;
        }
        let pending = cpu.has_expired_task_deadlines() || cpu.has_due_task_deadline(monotonic_now);
        cpu.as_mut().finish_task_deadline_softirq(pending);
        if pending {
            cpu.remote().publish_ktimer_work();
        }
        let update = cpu
            .as_mut()
            .next_scheduler_deadline_update_if_changed(monotonic_now)?;
        Ok(KtimerServiceBatch {
            processed,
            pending,
            update,
        })
    }

    fn service_buffered_expired_deadline(
        &self,
        cpu: Pin<&mut CpuLocal>,
        event: ExpiredTaskDeadline,
    ) -> Result<bool, TaskError> {
        match event.kind() {
            Some(TaskDeadlineKind::ParkTimeout { .. }) => {
                self.service_buffered_expired_park_deadline(cpu, event)
            }
            Some(TaskDeadlineKind::DeadlineCbs | TaskDeadlineKind::DeadlineZeroLag) => {
                task_runtime::fatal_invariant(0x444c_0011, event.token().generation() as usize)
            }
            None => Ok(false),
        }
    }

    fn service_buffered_expired_park_deadline(
        &self,
        cpu: Pin<&mut CpuLocal>,
        event: ExpiredTaskDeadline,
    ) -> Result<bool, TaskError> {
        let Some(thread) = event.thread() else {
            return Ok(false);
        };
        let handle = match self.thread_handle(thread) {
            Ok(handle) => Some(handle),
            Err(TaskError::StaleThreadId) => None,
            Err(error) => return Err(error),
        };
        let completed = {
            let mut deadline_base = cpu
                .remote()
                .lock_deadline_base(DeadlineBaseGuardSource::SoftExpiry);
            if deadline_base.take_buffered_event(event).is_none() {
                return Ok(false);
            }
            handle
                .as_ref()
                .is_some_and(|handle| handle.core.complete_sleep_timer(event.token().generation()))
        };
        let Some(handle) = handle else {
            return Ok(true);
        };
        let park_matches = event
            .kind()
            .is_some_and(|kind| kind.park_generation() == Some(handle.core.park_generation()));
        if completed && park_matches {
            let _wake_result = handle.wake_handle().wake();
        }
        Ok(true)
    }

    pub(super) fn service_expired_park_deadline(
        &self,
        event: ExpiredTaskDeadline,
    ) -> Result<(), TaskError> {
        let Some(thread) = event.thread() else {
            return Ok(());
        };
        let handle = match self.thread_handle(thread) {
            Ok(handle) => handle,
            Err(TaskError::StaleThreadId) => return Ok(()),
            Err(error) => return Err(error),
        };
        let completed = handle.core.complete_sleep_timer(event.token().generation());
        let park_matches = event
            .kind()
            .is_some_and(|kind| kind.park_generation() == Some(handle.core.park_generation()));
        if completed && park_matches {
            let _wake_result = handle.wake_handle().wake();
        }
        Ok(())
    }

    pub(crate) fn service_expired_scheduler_deadline(
        &self,
        cpu: Pin<&mut CpuLocal>,
        event: ExpiredTaskDeadline,
    ) -> Result<(), TaskError> {
        let Some(thread) = event.thread() else {
            return Ok(());
        };
        // Scheduler hard timers must not enter the task-only global registry.
        // The owner rq retains every admitted Deadline core until both timers
        // are cancelled and the reservation is detached, matching Linux's
        // sched_dl_entity-embedded hrtimer lifetime.
        let Some(core) = cpu.remote().lock_run_queue().deadline_member(thread) else {
            return Ok(());
        };
        match event.kind() {
            Some(TaskDeadlineKind::DeadlineCbs) => {
                self.service_expired_deadline_cbs(cpu, core, event)
            }
            Some(TaskDeadlineKind::DeadlineZeroLag) => {
                self.service_expired_deadline_zero_lag(cpu, core, event)
            }
            Some(TaskDeadlineKind::ParkTimeout { .. }) | None => Ok(()),
        }
    }

    /// Drains one bounded remainder of hard scheduler timers at an owner safe
    /// point after the timer IRQ transferred progress responsibility.
    ///
    /// Linux hard hrtimers run in interrupt context; ax-task additionally
    /// bounds that path. A budget overrun therefore remains scheduler work,
    /// never a rearmed overdue physical edge or arbitrary task callback.
    pub(super) fn service_due_scheduler_deadlines(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        now: MonotonicInstant,
        budget: usize,
    ) -> Result<bool, TaskError> {
        let mut processed = 0;
        let mut pending = false;
        while processed < budget {
            let (event, more) = cpu.as_mut().take_due_scheduler_deadline(now);
            pending = more;
            let Some(event) = event else {
                break;
            };
            self.service_expired_scheduler_deadline(cpu.as_mut(), event)?;
            processed += 1;
        }
        pending |= cpu.has_due_scheduler_deadline(now);
        if pending {
            cpu.request_reschedule();
        }
        Ok(pending)
    }

    fn service_expired_deadline_cbs(
        &self,
        cpu: Pin<&mut CpuLocal>,
        core: Arc<ThreadCore>,
        event: ExpiredTaskDeadline,
    ) -> Result<(), TaskError> {
        let owner = cpu.owner();
        let mut sched = core.sched().lock();
        if !Self::take_expired_registration(&mut sched.deadline.cbs_timer, event) {
            return Ok(());
        }
        if sched.deadline.bandwidth.reservation_owner() != Some(owner) {
            return Err(TaskError::CpuOwnerMismatch {
                expected: sched
                    .deadline
                    .bandwidth
                    .reservation_owner()
                    .map_or(u32::MAX, CpuId::as_u32),
                actual: owner.as_u32(),
            });
        }
        self.refresh_owner_deadline_timers_locked(&core, &mut sched, cpu);
        Ok(())
    }

    fn service_expired_deadline_zero_lag(
        &self,
        cpu: Pin<&mut CpuLocal>,
        core: Arc<ThreadCore>,
        event: ExpiredTaskDeadline,
    ) -> Result<(), TaskError> {
        let owner = cpu.owner();
        let mut sched = core.sched().lock();
        if !Self::take_expired_registration(&mut sched.deadline.zero_lag_timer, event) {
            return Ok(());
        }
        if sched.deadline.bandwidth.reservation_owner() != Some(owner) {
            return Err(TaskError::CpuOwnerMismatch {
                expected: sched
                    .deadline
                    .bandwidth
                    .reservation_owner()
                    .map_or(u32::MAX, CpuId::as_u32),
                actual: owner.as_u32(),
            });
        }
        self.refresh_owner_deadline_timers_locked(&core, &mut sched, cpu);
        Ok(())
    }

    pub(super) fn program_local_timer(&self, mut cpu: Pin<&mut CpuLocal>) -> Result<(), TaskError> {
        let monotonic_now = task_runtime::monotonic_now();
        let Some(update) = cpu
            .as_mut()
            .next_scheduler_deadline_update_if_changed(monotonic_now)?
        else {
            return Ok(());
        };
        task_runtime::publish_scheduler_deadline(update);
        Ok(())
    }
}
