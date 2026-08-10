//! Park, current-thread exit, and physical switch-tail completion.

#[cfg(test)]
use core::sync::atomic::Ordering;

use super::*;

#[cfg(test)]
static PARK_COMMIT_WAKE_RACE_ARMED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
#[cfg(test)]
static PARK_COMMIT_WAKE_RACE_SYSTEM: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static PARK_COMMIT_WAKE_RACE_THREAD: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);
#[cfg(test)]
static PARK_COMMIT_WAKE_RACE_ENTERED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
#[cfg(test)]
static PARK_COMMIT_WAKE_RACE_COMPLETED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

#[cfg(test)]
static PARK_AFTER_FINAL_WAKE_CHECK_ARMED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
#[cfg(test)]
static PARK_AFTER_FINAL_WAKE_CHECK_SYSTEM: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static PARK_AFTER_FINAL_WAKE_CHECK_THREAD: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);
#[cfg(test)]
static PARK_AFTER_FINAL_WAKE_CHECK_ENTERED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
#[cfg(test)]
static PARK_AFTER_FINAL_WAKE_CHECK_COMPLETED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

#[cfg(test)]
pub(super) fn arm_park_commit_wake_race(system: &TaskSystem, thread: ThreadId) {
    PARK_COMMIT_WAKE_RACE_ENTERED.store(false, Ordering::Release);
    PARK_COMMIT_WAKE_RACE_COMPLETED.store(false, Ordering::Release);
    assert!(
        !PARK_COMMIT_WAKE_RACE_ARMED.swap(true, Ordering::AcqRel),
        "only one deterministic park race may be armed"
    );
    PARK_COMMIT_WAKE_RACE_SYSTEM.store(
        (system as *const TaskSystem).expose_provenance(),
        Ordering::Release,
    );
    PARK_COMMIT_WAKE_RACE_THREAD.store(thread.as_u64(), Ordering::Release);
}

#[cfg(test)]
pub(super) fn park_commit_wake_race_entered() -> bool {
    PARK_COMMIT_WAKE_RACE_ENTERED.load(Ordering::Acquire)
}

#[cfg(test)]
pub(super) fn complete_park_commit_wake_race() {
    PARK_COMMIT_WAKE_RACE_COMPLETED.store(true, Ordering::Release);
}

#[cfg(test)]
fn park_commit_wake_race_hook(system: &TaskSystem, thread: ThreadId) {
    if PARK_COMMIT_WAKE_RACE_SYSTEM.load(Ordering::Acquire)
        != (system as *const TaskSystem).expose_provenance()
        || PARK_COMMIT_WAKE_RACE_THREAD.load(Ordering::Acquire) != thread.as_u64()
    {
        return;
    }
    if !PARK_COMMIT_WAKE_RACE_ARMED.swap(false, Ordering::AcqRel) {
        return;
    }
    PARK_COMMIT_WAKE_RACE_ENTERED.store(true, Ordering::Release);
    while !PARK_COMMIT_WAKE_RACE_COMPLETED.load(Ordering::Acquire) {
        core::hint::spin_loop();
    }
}

#[cfg(test)]
pub(super) fn arm_park_after_final_wake_check(system: &TaskSystem, thread: ThreadId) {
    PARK_AFTER_FINAL_WAKE_CHECK_ENTERED.store(false, Ordering::Release);
    PARK_AFTER_FINAL_WAKE_CHECK_COMPLETED.store(false, Ordering::Release);
    assert!(
        !PARK_AFTER_FINAL_WAKE_CHECK_ARMED.swap(true, Ordering::AcqRel),
        "only one deterministic post-check park race may be armed"
    );
    PARK_AFTER_FINAL_WAKE_CHECK_SYSTEM.store(
        (system as *const TaskSystem).expose_provenance(),
        Ordering::Release,
    );
    PARK_AFTER_FINAL_WAKE_CHECK_THREAD.store(thread.as_u64(), Ordering::Release);
}

#[cfg(test)]
pub(super) fn park_after_final_wake_check_entered() -> bool {
    PARK_AFTER_FINAL_WAKE_CHECK_ENTERED.load(Ordering::Acquire)
}

#[cfg(test)]
pub(super) fn complete_park_after_final_wake_check() {
    PARK_AFTER_FINAL_WAKE_CHECK_COMPLETED.store(true, Ordering::Release);
}

#[cfg(test)]
fn park_after_final_wake_check_hook(system: &TaskSystem, thread: ThreadId) {
    if PARK_AFTER_FINAL_WAKE_CHECK_SYSTEM.load(Ordering::Acquire)
        != (system as *const TaskSystem).expose_provenance()
        || PARK_AFTER_FINAL_WAKE_CHECK_THREAD.load(Ordering::Acquire) != thread.as_u64()
    {
        return;
    }
    if !PARK_AFTER_FINAL_WAKE_CHECK_ARMED.swap(false, Ordering::AcqRel) {
        return;
    }
    PARK_AFTER_FINAL_WAKE_CHECK_ENTERED.store(true, Ordering::Release);
    while !PARK_AFTER_FINAL_WAKE_CHECK_COMPLETED.load(Ordering::Acquire) {
        core::hint::spin_loop();
    }
}

pub(crate) struct CurrentExitPermit {
    thread: ThreadId,
    scheduler_exit: OwnedThreadSchedulerExit,
}

impl CurrentExitPermit {
    pub(crate) const fn thread(&self) -> ThreadId {
        self.thread
    }

    fn seal(&mut self) {
        self.scheduler_exit.seal();
    }
}

impl TaskSystem {
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
        core.sched()
            .lock()
            .transition(&core, ThreadState::Parking)?;
        Ok(ParkPrepare::Prepared(ParkTicket::new(
            core.id(),
            generation,
        )))
    }

    /// Rechecks a prepared park and either cancels it or commits schedule-out.
    pub fn commit_park(
        &self,
        cpu: Pin<&mut CpuLocal>,
        token: &mut ParkTicket,
    ) -> Result<ParkCommit, TaskError> {
        self.commit_park_owner(cpu, token, OwnerRqEntry::IrqSave)
    }

    /// Commits park while the runtime owns the IRQ-off scheduler baton.
    ///
    /// # Safety
    ///
    /// The scheduler frame must remain active until this function returns.
    pub(crate) unsafe fn commit_park_in_scheduler_frame(
        &self,
        cpu: Pin<&mut CpuLocal>,
        token: &mut ParkTicket,
    ) -> Result<ParkCommit, TaskError> {
        self.commit_park_owner(cpu, token, OwnerRqEntry::SchedulerFrame)
    }

    fn commit_park_owner(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        token: &mut ParkTicket,
        rq_entry: OwnerRqEntry,
    ) -> Result<ParkCommit, TaskError> {
        if token.is_resolved() {
            return Err(TaskError::StaleThreadId);
        }
        self.ensure_owner_cpu_context(&cpu)?;
        let remote = Arc::clone(cpu.remote());
        if remote.current_thread() != Some(token.thread()) {
            return Err(TaskError::StaleThreadId);
        }
        if let Some(registration) = token.deadline()
            && let Some(event) = cpu.as_mut().take_buffered_expiration(registration)
        {
            self.service_expired_park_deadline(event)?;
        }
        let initial_request = remote.claim_scheduler_request();
        self.drain_owner_work(cpu.as_mut())?;
        self.ensure_owner_cpu_online(&cpu)?;
        let previous_core_hint = cpu
            .current_core()
            .filter(|core| core.id() == token.thread())
            .ok_or(TaskError::StaleThreadId)?;
        #[cfg(test)]
        park_commit_wake_race_hook(self, previous_core_hint.id());
        let mut previous_sched = previous_core_hint.sched().lock();
        // SAFETY: propagated from the selected entry contract.
        let mut transaction = unsafe { rq_entry.begin(self, &remote) };
        transaction.adopt_scheduler_request(initial_request);
        let scheduler_request = transaction.merge_scheduler_request();
        let clock = transaction.clock();
        let now_ns = clock.wall().as_nanos();
        if transaction.current_thread() != Some(token.thread()) {
            transaction.commit_and_acknowledge_scheduler_request();
            return Err(TaskError::StaleThreadId);
        }
        let Some(previous_core) = transaction.current_core() else {
            transaction.commit_and_acknowledge_scheduler_request();
            return Err(TaskError::NoRunnableThread);
        };
        if !Arc::ptr_eq(&previous_core, &previous_core_hint) {
            transaction.commit_and_acknowledge_scheduler_request();
            return Err(TaskError::InvalidConfiguration);
        }
        let generation = previous_core.park_generation();
        if generation != token.generation() {
            transaction.commit_and_acknowledge_scheduler_request();
            return Err(TaskError::StaleThreadId);
        }
        let notified = previous_core.take_park_notification();
        if notified {
            previous_sched
                .transition(&previous_core, ThreadState::Running)
                .unwrap_or_else(|_| {
                    task_runtime::fatal_invariant(0x504b_1101, previous_core.id().as_u64() as usize)
                });
            cpu.finish_park_preemption(true);
            transaction.commit_and_acknowledge_scheduler_request();
            token.mark_resolved();
            return Ok(ParkCommit::Notified);
        }
        cpu.defer_park_preemption(scheduler_request.preempt_requested());
        let dispatch_commit = self.commit_owner_current_dispatch_in_rq(&mut transaction);
        let previous_endpoint = transaction.current_switch_endpoint().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x504b_1102, previous_core.id().as_u64() as usize)
        });
        let resumed = {
            let placement = previous_core.sched().placement();
            let sched = &mut *previous_sched;
            // This is the serialization edge shared with wake_thread_direct.
            // A wake that observes Parking publishes PARK_NOTIFIED while
            // holding this same lock. Rechecking and either restoring Running
            // or publishing Blocked in one transaction makes that wake the
            // unique winner instead of dropping it between two observations.
            if previous_core.take_park_notification() {
                sched
                    .transition(&previous_core, ThreadState::Running)
                    .unwrap_or_else(|_| {
                        task_runtime::fatal_invariant(
                            0x504b_1103,
                            previous_core.id().as_u64() as usize,
                        )
                    });
                true
            } else {
                #[cfg(test)]
                park_after_final_wake_check_hook(self, previous_core.id());
                if sched.lifecycle.state() != ThreadState::Parking
                    || placement.execution_cpu() != Some(cpu.owner())
                    || placement.on_cpu() != Some(cpu.owner())
                {
                    task_runtime::fatal_invariant(
                        0x504b_1104,
                        previous_core.id().as_u64() as usize,
                    );
                }
                // Timer replacement is the final recoverable preparation.
                // A wake cannot cross this point while the thread lock is
                // held; all following rq and placement changes are one owner
                // commit and cannot return a partial block.
                let active = if transaction.is_linked_current(previous_core.id()) {
                    transaction
                        .deactivate_task(previous_core.id())
                        .into_active()
                } else {
                    transaction.deactivate_unlinked_current(previous_core.id());
                    transaction
                        .take_current()
                        .and_then(CurrentDispatch::into_active)
                        .unwrap_or_else(|| {
                            task_runtime::fatal_invariant(
                                0x504b_1105,
                                previous_core.id().as_u64() as usize,
                            )
                        })
                };
                sched.policy.install_active(active);
                self.mark_owner_deadline_non_contending_in_rq(
                    &previous_core,
                    sched,
                    cpu.as_mut(),
                    now_ns,
                    &mut transaction,
                );
                let timing_granularity_ns = self.config.timing_granularity_ns();
                if let Some(fair) = sched.policy.active().base_entity().fair() {
                    let virtual_time = transaction.virtual_time_for_mode(fair.mode());
                    sched
                        .policy
                        .active_mut()
                        .base_entity_mut()
                        .capture_fair_sleep_lag(virtual_time, timing_granularity_ns);
                }
                sched
                    .transition(&previous_core, ThreadState::Blocked)
                    .unwrap_or_else(|_| {
                        task_runtime::fatal_invariant(
                            0x504b_1106,
                            previous_core.id().as_u64() as usize,
                        )
                    });
                placement.block_current(cpu.owner());
                false
            }
        };
        if resumed {
            transaction.commit_and_acknowledge_scheduler_request();
            drop(previous_sched);
            self.finish_owner_dispatch_commit(
                cpu.as_mut(),
                dispatch_commit,
                clock.wall().as_nanos(),
            );
            cpu.finish_park_preemption(true);
            token.mark_resolved();
            return Ok(ParkCommit::Notified);
        }
        cpu.finish_park_preemption(false);
        transaction.take_current();
        // This branch commits a real switch, so the request generated while
        // settling the outgoing dispatch belongs to this decision. The
        // resumed branch above deliberately leaves it for the next pass.
        transaction.merge_scheduler_request();
        let next = self.pick_owner_next_in_rq(cpu.as_mut(), &mut transaction, Some(token.thread()));
        let next_core = next.core;
        let next_endpoint = transaction.current_switch_endpoint().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x504b_1107, next_core.id().as_u64() as usize)
        });
        Self::stage_switch_handoff(
            cpu.as_mut(),
            Some(token.thread()),
            Some(Arc::clone(&previous_core)),
            next_core.id(),
            None,
        );
        transaction.commit_and_acknowledge_scheduler_request();
        drop(previous_sched);
        let decision = Self::owner_switch_plan(
            Some(&previous_core),
            Some(previous_endpoint),
            &next_core,
            next_endpoint,
            SwitchReason::Blocked,
            now_ns,
        );
        self.finish_owner_dispatch_commit(cpu.as_mut(), dispatch_commit, clock.wall().as_nanos());
        let decision = self.finish_owner_selection(cpu.as_mut(), decision);
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
        core.sched()
            .lock()
            .transition(&core, ThreadState::Running)?;
        cpu.finish_park_preemption(true);
        token.mark_resolved();
        Ok(())
    }

    /// Validates all fallible current-thread exit prerequisites without
    /// publishing the thread as exited.
    pub(crate) fn prepare_current_exit(
        &self,
        cpu: Pin<&mut CpuLocal>,
    ) -> Result<CurrentExitPermit, TaskError> {
        self.prepare_current_exit_inner(cpu, true)
    }

    pub(super) fn prepare_current_exit_inner(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        require_runtime_context: bool,
    ) -> Result<CurrentExitPermit, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        self.complete_context_switch(cpu.as_mut())?;
        self.drain_owner_work(cpu.as_mut())?;
        let current = cpu.current().ok_or(TaskError::NoRunnableThread)?;
        if cpu.idle() == Some(current) {
            return Err(TaskError::InvalidConfiguration);
        }
        let current_core = cpu.current_core().ok_or(TaskError::NoRunnableThread)?;
        // Close before taking registry or thread-state locks. An activity that
        // won before this edge may need either lock to finish, just as Linux
        // takes p->pi_lock before rq/task-state validation rather than waiting
        // for a reader while holding rq.
        let scheduler_exit = current_core
            .close_owned_scheduler_activity()
            .ok_or(TaskError::ThreadBusy)?;
        let state = self.state.lock();
        state.ensure_cpu_online(&cpu)?;
        let record = state.thread_record(current)?;
        if !Arc::ptr_eq(&record.core, &current_core) {
            return Err(TaskError::StaleThreadId);
        }
        let sched = record.sched.lock();
        let placement = record.sched.placement();
        let lifecycle = sched.lifecycle.state();
        if lifecycle != ThreadState::Running {
            return Err(TaskError::InvalidTransition {
                from: lifecycle,
                to: ThreadState::Exited,
            });
        }
        if sched.pi.blocked_on.is_some() || !sched.pi.donors.is_empty() {
            return Err(TaskError::InvalidPiState);
        }
        if placement.execution_cpu() != Some(cpu.owner()) || placement.on_cpu() != Some(cpu.owner())
        {
            return Err(TaskError::ThreadBusy);
        }
        if require_runtime_context && record.resources.context().is_none() {
            return Err(TaskError::InvalidRuntimeHandle);
        }
        record.callbacks.validate_prepare_exit()?;
        Ok(CurrentExitPermit {
            thread: current,
            scheduler_exit,
        })
    }

    /// Atomically prepares and commits current-thread exit.
    ///
    /// Runtime integrations that publish OS completion between those phases
    /// use the crate-private prepared form instead.
    pub fn exit_current(&self, mut cpu: Pin<&mut CpuLocal>) -> Result<ScheduleDecision, TaskError> {
        // Pure scheduler users may model a transition without installing an
        // architecture context. The runtime facade uses the stricter prepared
        // form before publishing OS-visible completion.
        let permit = self.prepare_current_exit_inner(cpu.as_mut(), false)?;
        self.commit_current_exit_after_owner_drain(cpu, permit)
    }

    /// Commits a prepared current-thread exit and selects a replacement.
    /// Commits a prepared exit while the runtime owns the IRQ-off scheduler baton.
    ///
    /// # Safety
    ///
    /// The scheduler frame must remain active until this function returns.
    pub(crate) unsafe fn commit_prepared_current_exit(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        permit: CurrentExitPermit,
    ) -> Result<ScheduleDecision, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        self.complete_context_switch(cpu.as_mut())?;
        self.drain_owner_work(cpu.as_mut())?;
        self.commit_current_exit_owner(cpu, permit, OwnerRqEntry::SchedulerFrame)
    }

    /// Commits the non-returning half of current exit after owner work drained.
    ///
    /// The move-only permit has already closed new scheduler activity. A
    /// message whose delivery reservation predates that close remains an
    /// in-flight late delivery and pins registry resources until its owner
    /// drains it as an exited no-op.
    pub(super) fn commit_current_exit_after_owner_drain(
        &self,
        cpu: Pin<&mut CpuLocal>,
        permit: CurrentExitPermit,
    ) -> Result<ScheduleDecision, TaskError> {
        self.commit_current_exit_owner(cpu, permit, OwnerRqEntry::IrqSave)
    }

    fn commit_current_exit_owner(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        mut permit: CurrentExitPermit,
        rq_entry: OwnerRqEntry,
    ) -> Result<ScheduleDecision, TaskError> {
        let exiting = permit.thread();
        let exited_core = {
            let state = self.state.lock();
            state.ensure_cpu_online(&cpu)?;
            let previous = cpu.current().ok_or(TaskError::NoRunnableThread)?;
            if previous != exiting {
                return Err(TaskError::StaleThreadId);
            }
            let previous_core = cpu.current_core();
            let record = state.thread_record(previous)?;
            if record.has_live_pi_edges() {
                return Err(TaskError::InvalidPiState);
            }
            record.callbacks.validate_prepare_exit()?;
            previous_core.ok_or(TaskError::NoRunnableThread)?
        };

        let remote = Arc::clone(cpu.remote());
        let initial_request = remote.claim_scheduler_request();
        let mut exited_sched = exited_core.sched().lock();
        // SAFETY: propagated from the selected entry contract.
        let mut transaction = unsafe { rq_entry.begin(self, &remote) };
        let clock = transaction.clock();
        let now_ns = clock.wall().as_nanos();
        if transaction.current_thread() != Some(exiting)
            || transaction
                .current_core()
                .is_none_or(|core| !Arc::ptr_eq(&core, &exited_core))
        {
            transaction.adopt_scheduler_request(initial_request);
            transaction.commit_and_acknowledge_scheduler_request();
            return Err(TaskError::StaleThreadId);
        }
        transaction.adopt_scheduler_request(initial_request);
        transaction.merge_scheduler_request();
        let dispatch_commit = self.commit_owner_current_dispatch_in_rq(&mut transaction);
        // Exit necessarily selects a replacement, so accounting requests from
        // the outgoing task are consumed by this decision.
        transaction.merge_scheduler_request();
        let previous_endpoint = transaction.current_switch_endpoint().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x4558_0007, exiting.as_u64() as usize)
        });
        let held_reservation = {
            let placement = exited_core.sched().placement();
            let sched = &mut *exited_sched;
            if sched.lifecycle.state() != ThreadState::Running
                || placement.execution_cpu() != Some(cpu.owner())
                || placement.on_cpu() != Some(cpu.owner())
            {
                task_runtime::fatal_invariant(0x4558_1101, exiting.as_u64() as usize);
            }
            Self::detach_owner_deadline_bandwidth_in_rq(
                &exited_core,
                sched,
                cpu.remote(),
                &mut transaction,
            );
            if transaction.is_linked_current(exiting) {
                transaction.deactivate_task(exiting);
            } else {
                transaction.deactivate_unlinked_current(exiting);
            }
            if sched.transition(&exited_core, ThreadState::Exited).is_err() {
                task_runtime::fatal_invariant(0x4558_0001, exiting.as_u64() as usize);
            }
            // Exit removes rq ownership immediately. The outgoing execution
            // claim remains in `on_cpu` until the per-CPU switch handoff tail
            // releases it, exactly like Linux `do_task_dead()` followed by
            // `finish_task_switch()`.
            placement.block_current(cpu.owner());
            permit.seal();
            let held = sched.held_deadline_reservation();
            sched.deadline.bandwidth.replace_detached_reservation(0);
            sched.policy.discard_pending_update();
            held
        };
        transaction.take_current();
        let next = self.pick_owner_next_in_rq(cpu.as_mut(), &mut transaction, Some(exiting));
        let next_core = next.core;
        let next_endpoint = transaction.current_switch_endpoint().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x4558_0008, exiting.as_u64() as usize)
        });
        Self::stage_switch_handoff(
            cpu.as_mut(),
            Some(exiting),
            Some(Arc::clone(&exited_core)),
            next_core.id(),
            None,
        );
        transaction.commit_and_acknowledge_scheduler_request();
        drop(exited_sched);
        let decision = Self::owner_switch_plan(
            Some(&exited_core),
            Some(previous_endpoint),
            &next_core,
            next_endpoint,
            SwitchReason::Exited,
            now_ns,
        );
        self.finish_owner_dispatch_commit(cpu.as_mut(), dispatch_commit, clock.wall().as_nanos());

        {
            let mut state = self.state.lock();
            let record = state.thread_record_mut(exiting).unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x4558_0002, exiting.as_u64() as usize)
            });
            if record
                .callbacks
                .prepare_exit(record.extension.is_some())
                .is_err()
            {
                task_runtime::fatal_invariant(0x4558_0003, exiting.as_u64() as usize);
            }
            state.queue_exited_thread(exiting);
        }
        self.root_domain.lock().release_deadline(held_reservation);
        exited_core.notify_affinity_waiters();
        drop(permit);
        let decision = self.finish_owner_selection(cpu.as_mut(), decision);
        Ok(decision)
    }

    /// Completes the physical switch-out handoff in the newly active context.
    ///
    /// This second phase clears `on_cpu` only after architecture execution has
    /// left the previous stack. Deferred migration publication and exit hooks
    /// therefore cannot make a context runnable or reapable too early.
    #[doc(hidden)]
    pub fn complete_context_switch(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
    ) -> Result<SwitchInCompletion, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        let Some(initial_handoff) = cpu.as_ref().get_ref().switch_handoff() else {
            return Ok(SwitchInCompletion::NONE);
        };
        let owner = cpu.owner();
        let previous_core = Arc::clone(initial_handoff.previous());
        let migration_target = initial_handoff.migration_target();
        let runtime_tail_finished = initial_handoff.runtime_tail_is_finished();
        {
            let placement = previous_core.sched().placement();
            let sched = previous_core.sched().lock();
            let remote = Arc::clone(cpu.remote());
            let transaction = OwnerRqTxn::begin(self, &remote);
            let validation = self.validate_switch_handoff_state(
                owner,
                transaction.deadline_bandwidth(),
                initial_handoff,
                placement,
                &sched,
            );
            if let Err(error) = validation {
                transaction.commit();
                return Err(error);
            }
            transaction.commit();
        }

        if !runtime_tail_finished {
            task_runtime::finish_context_switch_tail();
            if cpu
                .as_mut()
                .finish_switch_runtime_tail(previous_core.id(), migration_target)
                .is_err()
            {
                task_runtime::fatal_invariant(0x5357_0001, previous_core.id().as_u64() as usize);
            }
        }

        let handoff = cpu
            .as_ref()
            .get_ref()
            .switch_handoff()
            .ok_or(TaskError::InvalidConfiguration)?;
        let previous = handoff.previous().id();
        let incoming = cpu.current_core().ok_or(TaskError::NoRunnableThread)?;
        if incoming.id() == previous {
            return Err(TaskError::InvalidConfiguration);
        }
        let (migration_target, previous_exited, wake_after_tail, affinity_completed) = {
            let placement = previous_core.sched().placement();
            let mut sched = handoff.previous().sched().lock();
            let remote = Arc::clone(cpu.remote());
            let mut transaction = OwnerRqTxn::begin(self, &remote);
            let validation = self.validate_switch_handoff_state(
                owner,
                transaction.deadline_bandwidth(),
                handoff,
                placement,
                &sched,
            );
            let (migration_target, previous_exited) = match validation {
                Ok(validated) => validated,
                Err(error) => {
                    transaction.commit();
                    return Err(error);
                }
            };
            if migration_target.is_some() && sched.deadline.bandwidth.reservation_owner().is_some()
            {
                Self::detach_owner_deadline_bandwidth_in_rq(
                    &previous_core,
                    &mut sched,
                    &remote,
                    &mut transaction,
                );
            }
            // Linux `finish_task_switch()` clears `prev->on_cpu` before
            // releasing `rq->lock`; wake, migration, and reaping therefore
            // cannot observe a released execution claim with stale rq state.
            placement.finish_task(owner);
            transaction.commit();
            if let Some(target) = migration_target {
                previous_core.set_wake_cpu_hint(target);
            }
            let affinity_completed =
                Self::complete_affinity_if_satisfied_locked(&previous_core, &sched);
            (
                migration_target,
                previous_exited,
                sched.lifecycle.state() == ThreadState::Waking,
                affinity_completed,
            )
        };
        if affinity_completed {
            previous_core.notify_affinity_waiters();
        }
        let consumed = cpu.as_mut().take_switch_handoff().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x5357_0003, previous.as_u64() as usize)
        });
        if consumed.previous().id() != previous || consumed.migration_target() != migration_target {
            task_runtime::fatal_invariant(0x5357_0004, previous.as_u64() as usize);
        }
        let (_, migration) = consumed.into_runtime_finished().unwrap_or_else(|_| {
            task_runtime::fatal_invariant(0x5357_0004, previous.as_u64() as usize)
        });
        if let Some(migration) = migration {
            migration.commit();
        }
        if wake_after_tail {
            self.finish_switch_tail_wake(&previous_core);
        }
        if previous_exited {
            self.task_work.publish();
        }
        Ok(SwitchInCompletion::for_core(&incoming))
    }

    fn validate_switch_handoff_state(
        &self,
        owner: CpuId,
        bandwidth: DeadlineBandwidthSnapshot,
        handoff: &crate::system::cpu::SwitchHandoff,
        placement: &crate::system::thread_sched::SchedulerPlacement,
        sched: &ThreadSchedState,
    ) -> Result<(Option<CpuId>, bool), TaskError> {
        if placement.on_cpu() != Some(owner) {
            return Err(TaskError::InvalidConfiguration);
        }
        let migration_target = match handoff.migration_target() {
            Some(reserved_target) => {
                let target = placement
                    .committed_migration_target()
                    .ok_or(TaskError::InvalidConfiguration)?;
                if target != reserved_target {
                    return Err(TaskError::InvalidConfiguration);
                }
                if sched.lifecycle.state() != ThreadState::Ready
                    || placement.queued_cpu().is_some()
                    || placement.execution_cpu().is_some()
                {
                    return Err(TaskError::InvalidConfiguration);
                }
                if let Some(assigned) = sched.deadline.bandwidth.reservation_owner() {
                    if assigned != owner {
                        return Err(TaskError::CpuOwnerMismatch {
                            expected: assigned.as_u32(),
                            actual: owner.as_u32(),
                        });
                    }
                    let reservation_scaled = sched.deadline.bandwidth.reservation_scaled();
                    if bandwidth.this_bw_scaled() < reservation_scaled
                        || (sched.deadline.bandwidth.is_active()
                            && bandwidth.running_bw_scaled() < reservation_scaled)
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
}
