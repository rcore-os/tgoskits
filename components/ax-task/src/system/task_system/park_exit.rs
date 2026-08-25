//! Park, current-thread exit, and physical switch-tail completion.

#[cfg(any(test, all(axtest, feature = "axtest"), feature = "task-test-hooks"))]
use core::sync::atomic::Ordering;

use super::*;
use crate::ParkPublication;

#[cfg(any(test, all(axtest, feature = "axtest")))]
static PARK_COMMIT_WAKE_RACE_ARMED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
#[cfg(any(test, all(axtest, feature = "axtest")))]
static PARK_COMMIT_WAKE_RACE_SYSTEM: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(any(test, all(axtest, feature = "axtest")))]
static PARK_COMMIT_WAKE_RACE_THREAD: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);
#[cfg(any(test, all(axtest, feature = "axtest")))]
static PARK_COMMIT_WAKE_RACE_ENTERED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
#[cfg(any(test, all(axtest, feature = "axtest")))]
static PARK_COMMIT_WAKE_RACE_COMPLETED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

#[cfg(any(test, all(axtest, feature = "axtest"), feature = "task-test-hooks"))]
static PARK_AFTER_FINAL_WAKE_CHECK_ARMED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
#[cfg(any(test, all(axtest, feature = "axtest"), feature = "task-test-hooks"))]
static PARK_AFTER_FINAL_WAKE_CHECK_SYSTEM: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(any(test, all(axtest, feature = "axtest"), feature = "task-test-hooks"))]
static PARK_AFTER_FINAL_WAKE_CHECK_THREAD: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);
#[cfg(any(test, all(axtest, feature = "axtest"), feature = "task-test-hooks"))]
static PARK_AFTER_FINAL_WAKE_CHECK_ENTERED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
#[cfg(any(test, all(axtest, feature = "axtest"), feature = "task-test-hooks"))]
static PARK_AFTER_FINAL_WAKE_CHECK_COMPLETED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

#[cfg(feature = "task-test-hooks")]
static PARK_AFTER_BLOCKED_PUBLICATION_ARMED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
#[cfg(feature = "task-test-hooks")]
static PARK_AFTER_BLOCKED_PUBLICATION_SYSTEM: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(feature = "task-test-hooks")]
static PARK_AFTER_BLOCKED_PUBLICATION_THREAD: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);
#[cfg(feature = "task-test-hooks")]
static PARK_AFTER_BLOCKED_PUBLICATION_ENTERED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
#[cfg(feature = "task-test-hooks")]
static PARK_AFTER_BLOCKED_PUBLICATION_COMPLETED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

#[cfg(feature = "task-test-hooks")]
static PARK_BEFORE_ACTIVE_PUBLICATION_ARMED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
#[cfg(feature = "task-test-hooks")]
static PARK_BEFORE_ACTIVE_PUBLICATION_SYSTEM: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(feature = "task-test-hooks")]
static PARK_BEFORE_ACTIVE_PUBLICATION_THREAD: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);
#[cfg(feature = "task-test-hooks")]
static PARK_BEFORE_ACTIVE_PUBLICATION_ENTERED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
#[cfg(feature = "task-test-hooks")]
static PARK_BEFORE_ACTIVE_PUBLICATION_COMPLETED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

#[cfg(any(test, all(axtest, feature = "axtest")))]
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

#[cfg(any(test, all(axtest, feature = "axtest")))]
pub(super) fn park_commit_wake_race_entered() -> bool {
    PARK_COMMIT_WAKE_RACE_ENTERED.load(Ordering::Acquire)
}

#[cfg(any(test, all(axtest, feature = "axtest")))]
pub(super) fn complete_park_commit_wake_race() {
    PARK_COMMIT_WAKE_RACE_COMPLETED.store(true, Ordering::Release);
}

#[cfg(any(test, all(axtest, feature = "axtest")))]
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

#[cfg(any(test, all(axtest, feature = "axtest"), feature = "task-test-hooks"))]
pub(crate) fn arm_park_after_final_wake_check(system: &TaskSystem, thread: ThreadId) {
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

#[cfg(any(test, all(axtest, feature = "axtest"), feature = "task-test-hooks"))]
pub(crate) fn park_after_final_wake_check_entered() -> bool {
    PARK_AFTER_FINAL_WAKE_CHECK_ENTERED.load(Ordering::Acquire)
}

#[cfg(any(test, all(axtest, feature = "axtest"), feature = "task-test-hooks"))]
pub(crate) fn complete_park_after_final_wake_check() {
    PARK_AFTER_FINAL_WAKE_CHECK_COMPLETED.store(true, Ordering::Release);
}

#[cfg(any(test, all(axtest, feature = "axtest"), feature = "task-test-hooks"))]
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

#[cfg(feature = "task-test-hooks")]
pub(crate) fn arm_park_after_blocked_publication(system: &TaskSystem, thread: ThreadId) {
    PARK_AFTER_BLOCKED_PUBLICATION_ENTERED.store(false, Ordering::Release);
    PARK_AFTER_BLOCKED_PUBLICATION_COMPLETED.store(false, Ordering::Release);
    assert!(
        !PARK_AFTER_BLOCKED_PUBLICATION_ARMED.swap(true, Ordering::AcqRel),
        "only one deterministic blocked-publication park race may be armed"
    );
    PARK_AFTER_BLOCKED_PUBLICATION_SYSTEM.store(
        (system as *const TaskSystem).expose_provenance(),
        Ordering::Release,
    );
    PARK_AFTER_BLOCKED_PUBLICATION_THREAD.store(thread.as_u64(), Ordering::Release);
}

#[cfg(feature = "task-test-hooks")]
pub(crate) fn park_after_blocked_publication_entered() -> bool {
    PARK_AFTER_BLOCKED_PUBLICATION_ENTERED.load(Ordering::Acquire)
}

#[cfg(feature = "task-test-hooks")]
pub(crate) fn complete_park_after_blocked_publication() {
    PARK_AFTER_BLOCKED_PUBLICATION_COMPLETED.store(true, Ordering::Release);
}

#[cfg(feature = "task-test-hooks")]
fn park_after_blocked_publication_hook(system: &TaskSystem, thread: ThreadId) {
    if PARK_AFTER_BLOCKED_PUBLICATION_SYSTEM.load(Ordering::Acquire)
        != (system as *const TaskSystem).expose_provenance()
        || PARK_AFTER_BLOCKED_PUBLICATION_THREAD.load(Ordering::Acquire) != thread.as_u64()
    {
        return;
    }
    if !PARK_AFTER_BLOCKED_PUBLICATION_ARMED.swap(false, Ordering::AcqRel) {
        return;
    }
    PARK_AFTER_BLOCKED_PUBLICATION_ENTERED.store(true, Ordering::Release);
    while !PARK_AFTER_BLOCKED_PUBLICATION_COMPLETED.load(Ordering::Acquire) {
        core::hint::spin_loop();
    }
}

#[cfg(feature = "task-test-hooks")]
pub(crate) fn arm_park_before_active_publication(system: &TaskSystem, thread: ThreadId) {
    PARK_BEFORE_ACTIVE_PUBLICATION_ENTERED.store(false, Ordering::Release);
    PARK_BEFORE_ACTIVE_PUBLICATION_COMPLETED.store(false, Ordering::Release);
    assert!(
        !PARK_BEFORE_ACTIVE_PUBLICATION_ARMED.swap(true, Ordering::AcqRel),
        "only one deterministic pre-publication park race may be armed"
    );
    PARK_BEFORE_ACTIVE_PUBLICATION_SYSTEM.store(
        (system as *const TaskSystem).expose_provenance(),
        Ordering::Release,
    );
    PARK_BEFORE_ACTIVE_PUBLICATION_THREAD.store(thread.as_u64(), Ordering::Release);
}

#[cfg(feature = "task-test-hooks")]
pub(crate) fn park_before_active_publication_entered() -> bool {
    PARK_BEFORE_ACTIVE_PUBLICATION_ENTERED.load(Ordering::Acquire)
}

#[cfg(feature = "task-test-hooks")]
pub(crate) fn complete_park_before_active_publication() {
    PARK_BEFORE_ACTIVE_PUBLICATION_COMPLETED.store(true, Ordering::Release);
}

#[cfg(feature = "task-test-hooks")]
fn park_before_active_publication_hook(system: &TaskSystem, thread: ThreadId) {
    if PARK_BEFORE_ACTIVE_PUBLICATION_SYSTEM.load(Ordering::Acquire)
        != (system as *const TaskSystem).expose_provenance()
        || PARK_BEFORE_ACTIVE_PUBLICATION_THREAD.load(Ordering::Acquire) != thread.as_u64()
    {
        return;
    }
    if !PARK_BEFORE_ACTIVE_PUBLICATION_ARMED.swap(false, Ordering::AcqRel) {
        return;
    }
    PARK_BEFORE_ACTIVE_PUBLICATION_ENTERED.store(true, Ordering::Release);
    while !PARK_BEFORE_ACTIVE_PUBLICATION_COMPLETED.load(Ordering::Acquire) {
        core::hint::spin_loop();
    }
}

pub(crate) struct CurrentExitPermit {
    scheduler_exit: OwnedThreadSchedulerExit,
    current_core: Arc<ThreadCore>,
}

impl CurrentExitPermit {
    pub(crate) fn thread(&self) -> ThreadId {
        self.current_core.id()
    }

    fn current_core(&self) -> &Arc<ThreadCore> {
        &self.current_core
    }

    fn seal(&mut self) {
        self.scheduler_exit.seal();
    }
}

impl TaskSystem {
    /// Publishes `PARKING` after consuming a wake-before-park notification.
    pub fn prepare_park(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        current: &ThreadHandle,
    ) -> Result<ParkPrepare, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        self.complete_context_switch(cpu.as_mut())?;
        self.ensure_owner_cpu_online(&cpu)?;
        let core = current.runtime_core_arc();
        let placement = core.sched().placement();
        if placement.queued_cpu() != Some(cpu.owner()) || placement.on_cpu() != Some(cpu.owner()) {
            return Err(TaskError::StaleThreadId);
        }
        self.prepare_current_park(core)
    }

    /// Publishes the current task's wait state before its later schedule pass.
    ///
    /// The runtime's current-thread publication is the architecture-context
    /// identity, like Linux `current`. Resumed and fresh task contexts complete
    /// switch tail before calling task code, so this state publication neither
    /// reclaims `CpuLocal` nor repeats switch-tail completion.
    pub(crate) fn prepare_current_park(
        &self,
        current: &Arc<ThreadCore>,
    ) -> Result<ParkPrepare, TaskError> {
        let core = Arc::clone(current);
        let placement = core.sched().placement();
        let queued_cpu = placement.queued_cpu();
        if core.state() != ThreadState::Running
            || queued_cpu.is_none()
            || placement.on_cpu() != queued_cpu
        {
            return Err(TaskError::StaleThreadId);
        }
        if core.take_park_notification() {
            return Ok(ParkPrepare::Notified);
        }
        let generation = core.next_park_generation()?;
        core.transition_state(ThreadState::Parking)?;
        Ok(ParkPrepare::Prepared(ParkTicket::new(
            core.id(),
            generation,
        )))
    }

    /// Rechecks a prepared park and either cancels it or commits schedule-out.
    pub fn commit_park(
        &self,
        cpu: Pin<&mut CpuLocal>,
        current: &ThreadHandle,
        token: &mut ParkTicket,
    ) -> Result<ParkCommit, TaskError> {
        self.commit_park_owner(
            cpu,
            current.runtime_core_arc(),
            token,
            OwnerRqEntry::IrqSave,
        )
    }

    /// Commits park while the runtime owns the IRQ-off scheduler baton.
    ///
    /// # Safety
    ///
    /// The scheduler frame must remain active until this function returns.
    pub(crate) unsafe fn commit_park_in_scheduler_frame(
        &self,
        cpu: Pin<&mut CpuLocal>,
        current: &Arc<ThreadCore>,
        token: &mut ParkTicket,
    ) -> Result<ParkCommit, TaskError> {
        self.commit_park_owner(cpu, current, token, OwnerRqEntry::SchedulerFrame)
    }

    fn commit_park_owner(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        current: &Arc<ThreadCore>,
        token: &mut ParkTicket,
        rq_entry: OwnerRqEntry,
    ) -> Result<ParkCommit, TaskError> {
        #[cfg(feature = "task-test-hooks")]
        crate::task_test_hooks::record_park_profile_stage(0);
        if token.is_resolved() || current.id() != token.thread() {
            return Err(TaskError::StaleThreadId);
        }
        self.ensure_owner_cpu_context(&cpu)?;
        let remote = Arc::clone(cpu.remote());
        if let Some(registration) = token.deadline()
            && let Some(event) = cpu.as_mut().take_buffered_expiration(registration)
        {
            self.service_expired_park_deadline(event)?;
        }
        let initial_request = remote.claim_scheduler_request(SchedulerRequestScope::All);
        self.drain_owner_work(cpu.as_mut())?;
        self.ensure_owner_cpu_online(&cpu)?;
        let previous_core_hint = Arc::clone(current);
        #[cfg(any(test, all(axtest, feature = "axtest")))]
        park_commit_wake_race_hook(self, previous_core_hint.id());
        #[cfg(feature = "task-test-hooks")]
        crate::task_test_hooks::record_park_profile_stage(1);
        if matches!(
            previous_core_hint.effective_policy_snapshot(),
            SchedulePolicy::Fifo { .. } | SchedulePolicy::RoundRobin { .. }
        ) && !previous_core_hint
            .sched()
            .placement()
            .has_pending_migration()
            && let Some(commit) = self.try_commit_park_rt_in_rq(
                cpu.as_mut(),
                token,
                &remote,
                &previous_core_hint,
                initial_request,
                rq_entry,
            )?
        {
            return Ok(commit);
        }
        #[cfg(feature = "task-test-hooks")]
        crate::task_test_hooks::record_park_thread_sched_acquisition(previous_core_hint.id());
        // SAFETY: propagated from the selected entry contract.
        let mut previous_sched = unsafe { rq_entry.lock_thread_sched(previous_core_hint.sched()) };
        // SAFETY: propagated from the selected entry contract.
        let mut transaction = unsafe { rq_entry.begin(self, &remote) };
        #[cfg(feature = "task-test-hooks")]
        crate::task_test_hooks::record_park_irq_owner_scopes(
            previous_core_hint.id(),
            previous_sched.owns_runtime_irq_scope(),
            transaction.owns_runtime_irq_scope(),
        );
        transaction.adopt_scheduler_request(initial_request);
        let scheduler_request = transaction.merge_scheduler_request(SchedulerRequestScope::All);
        let clock = transaction.clock();
        let now_ns = clock.wall().as_nanos();
        #[cfg(feature = "task-test-hooks")]
        crate::task_test_hooks::record_park_profile_stage(2);
        if transaction.current_thread() != Some(token.thread()) {
            transaction.commit_and_finish_scheduler_request();
            return Err(TaskError::StaleThreadId);
        }
        let Some(previous_core) = transaction.current_core() else {
            transaction.commit_and_finish_scheduler_request();
            return Err(TaskError::NoRunnableThread);
        };
        if !Arc::ptr_eq(&previous_core, &previous_core_hint) {
            transaction.commit_and_finish_scheduler_request();
            return Err(TaskError::InvalidConfiguration);
        }
        let generation = previous_core.park_generation();
        if generation != token.generation() {
            transaction.commit_and_finish_scheduler_request();
            return Err(TaskError::StaleThreadId);
        }
        let notified = previous_core.take_park_notification();
        if notified {
            previous_sched
                .transition(&previous_core, ThreadState::Running)
                .unwrap_or_else(|_| {
                    task_runtime::fatal_invariant(0x504b_1101, previous_core.id().as_u64() as usize)
                });
            // Linux restores TASK_RUNNING and still runs `schedule()` in
            // `__schedule()`, so a preemption request that raced the park is
            // served by the upcoming safe point instead of vanishing with the
            // claimed request word.
            cpu.defer_park_preemption(scheduler_request);
            cpu.finish_park_preemption(true);
            transaction.commit_and_finish_scheduler_request();
            token.mark_resolved();
            return Ok(ParkCommit::Notified);
        }
        cpu.defer_park_preemption(scheduler_request);
        let dispatch_commit = self.settle_owner_current_dispatch_in_rq(&mut transaction);
        #[cfg(feature = "task-test-hooks")]
        crate::task_test_hooks::record_park_profile_stage(3);
        let previous_endpoint = transaction.current_switch_endpoint().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x504b_1102, previous_core.id().as_u64() as usize)
        });
        let resumed = {
            let placement = previous_core.sched().placement();
            let sched = &mut *previous_sched;
            #[cfg(any(test, all(axtest, feature = "axtest"), feature = "task-test-hooks"))]
            park_after_final_wake_check_hook(self, previous_core.id());
            // Lifecycle and wake publication share one atomic word. A wake
            // that observes Parking sets PARK_NOTIFIED in that word; this CAS
            // either consumes it and restores Running or uniquely publishes
            // Blocked before a later waker enters the task-lock activation
            // path.
            if previous_core
                .publish_blocked_from_parking()
                .unwrap_or_else(|_| {
                    task_runtime::fatal_invariant(0x504b_1103, previous_core.id().as_u64() as usize)
                })
                == ParkPublication::Notified
            {
                true
            } else {
                if sched.lifecycle.state() != ThreadState::Blocked
                    || placement.queued_cpu() != Some(cpu.owner())
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
                #[cfg(feature = "task-test-hooks")]
                let force_delayed =
                    crate::task_test_hooks::force_fair_delay_dequeue(previous_core.id(), false);
                #[cfg(not(feature = "task-test-hooks"))]
                let force_delayed = false;
                let timing_granularity_ns = self.config.timing_granularity_ns();
                let delayed = !transaction.is_linked_current(previous_core.id())
                    && transaction
                        .delay_dequeue_unlinked_current(
                            previous_core.id(),
                            timing_granularity_ns,
                            force_delayed,
                        )
                        .is_some();
                if delayed {
                    placement.delay_dequeue_current(cpu.owner());
                } else {
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
                    previous_core.sched().install_active(sched, active);
                }
                self.mark_owner_deadline_non_contending_in_rq(
                    &previous_core,
                    sched,
                    cpu.as_mut(),
                    now_ns,
                    &mut transaction,
                );
                if !delayed {
                    let mut active = previous_core.sched().active(sched);
                    if active.base_entity().fair().is_some() {
                        let virtual_time = transaction.virtual_time();
                        active
                            .base_entity_mut()
                            .capture_fair_sleep_lag(virtual_time, timing_granularity_ns);
                    }
                }
                if !delayed {
                    placement.block_current(cpu.owner());
                }
                false
            }
        };
        if resumed {
            transaction.commit_and_finish_scheduler_request();
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
        #[cfg(feature = "task-test-hooks")]
        crate::task_test_hooks::record_park_profile_stage(4);
        cpu.finish_park_preemption(false);
        transaction.take_current();
        // This branch commits a real switch, so the request generated while
        // settling the outgoing dispatch belongs to this decision. The
        // resumed branch above deliberately leaves it for the next pass.
        transaction.merge_scheduler_request(SchedulerRequestScope::All);
        #[cfg(feature = "task-test-hooks")]
        crate::task_test_hooks::record_park_profile_stage(5);
        let next = self.pick_owner_next_in_rq(
            cpu.as_mut(),
            &mut transaction,
            Some((&previous_core, &mut previous_sched)),
        );
        let next_core = next.core;
        let next_endpoint = transaction.current_switch_endpoint().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x504b_1107, next_core.id().as_u64() as usize)
        });
        Self::stage_switch_handoff(
            cpu.as_mut(),
            Some(token.thread()),
            Some(Arc::clone(&previous_core)),
            Arc::clone(&next_core),
            None,
        );
        #[cfg(feature = "task-test-hooks")]
        crate::task_test_hooks::record_park_profile_stage(11);
        let deadline_rq_observation =
            transaction.scheduler_deadline_rq_observation(cpu.as_ref().get_ref());
        #[cfg(feature = "task-test-hooks")]
        crate::task_test_hooks::record_park_profile_stage(12);
        self.commit_owner_switch_selection(
            cpu.as_mut(),
            transaction,
            !dispatch_commit.has_deferred_task_lock_work(),
        );
        #[cfg(feature = "task-test-hooks")]
        crate::task_test_hooks::record_park_profile_stage(13);
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
        let decision = self.finish_owner_selection(cpu.as_mut(), decision, deadline_rq_observation);
        #[cfg(feature = "task-test-hooks")]
        crate::task_test_hooks::record_park_profile_stage(14);
        token.mark_resolved();
        Ok(ParkCommit::Blocked(decision))
    }

    /// Implements Linux's ordinary FIFO/RR `__schedule()` block transition.
    ///
    /// A non-PI RT current remains linked in its rq class node, so `rq->lock`
    /// provides the class mutation boundary exactly as it does in Linux
    /// `__schedule()`. Task-control writers retain the `task lock -> rq` order;
    /// this path never acquires the task lock in reverse. Instead, a move-only
    /// marker makes task-lock readers wait while rq removal, placement, and the
    /// detached entity owner are published as one transition. Special classes,
    /// PI, Deadline bandwidth, and migration use the full path directly.
    fn try_commit_park_rt_in_rq(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        token: &mut ParkTicket,
        remote: &Arc<CpuRemote>,
        previous_core: &Arc<ThreadCore>,
        initial_request: crate::system::cpu::SchedulerRequestClaim,
        rq_entry: OwnerRqEntry,
    ) -> Result<Option<ParkCommit>, TaskError> {
        let owner = cpu.owner();
        let placement = previous_core.sched().placement();
        // SAFETY: propagated from `commit_park_owner`'s selected entry
        // contract. The returned transaction does not outlive this helper.
        let mut transaction = unsafe { rq_entry.begin(self, remote) };
        let eligible = transaction.current().is_some_and(|current| {
            current.thread() == token.thread()
                && Arc::ptr_eq(current.runtime_core_arc(), previous_core)
                && current.is_rt()
                && !current.rt_quota_exempt()
                && current.metadata().deadline_bandwidth_scaled == 0
        }) && transaction.is_linked_current(previous_core.id())
            && previous_core.state() == ThreadState::Parking
            && placement.queued_cpu() == Some(owner)
            && placement.on_cpu() == Some(owner)
            && !placement.has_pending_migration();
        if !eligible {
            transaction.commit();
            return Ok(None);
        }

        #[cfg(feature = "task-test-hooks")]
        crate::task_test_hooks::record_park_irq_owner_scopes(
            previous_core.id(),
            false,
            transaction.owns_runtime_irq_scope(),
        );
        transaction.adopt_scheduler_request(initial_request);
        let scheduler_request = transaction.merge_scheduler_request(SchedulerRequestScope::All);
        let clock = transaction.clock();
        let now_ns = clock.wall().as_nanos();
        #[cfg(feature = "task-test-hooks")]
        crate::task_test_hooks::record_park_profile_stage(2);

        if previous_core.park_generation() != token.generation() {
            transaction.commit_and_finish_scheduler_request();
            return Err(TaskError::StaleThreadId);
        }
        if previous_core.take_park_notification() {
            previous_core
                .transition_state(ThreadState::Running)
                .unwrap_or_else(|_| {
                    task_runtime::fatal_invariant(0x504b_1111, previous_core.id().as_u64() as usize)
                });
            // Same ownership as the full-path early Notified branch: the race
            // that cancelled this park must not swallow the already claimed
            // preemption request.
            cpu.defer_park_preemption(scheduler_request);
            cpu.finish_park_preemption(true);
            transaction.commit_and_finish_scheduler_request();
            token.mark_resolved();
            return Ok(Some(ParkCommit::Notified));
        }

        cpu.defer_park_preemption(scheduler_request);
        let dispatch_commit = self.settle_owner_current_dispatch_in_rq(&mut transaction);
        #[cfg(feature = "task-test-hooks")]
        crate::task_test_hooks::record_park_profile_stage(3);
        if dispatch_commit.has_deferred_task_lock_work() {
            task_runtime::fatal_invariant(0x504b_1112, previous_core.id().as_u64() as usize);
        }
        let previous_endpoint = transaction.current_switch_endpoint().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x504b_1113, previous_core.id().as_u64() as usize)
        });
        #[cfg(feature = "task-test-hooks")]
        park_before_active_publication_hook(self, previous_core.id());
        let publication = previous_core
            .sched()
            .begin_active_publication()
            .unwrap_or_else(|| {
                task_runtime::fatal_invariant(0x504b_1119, previous_core.id().as_u64() as usize)
            });
        #[cfg(feature = "task-test-hooks")]
        crate::task_test_hooks::record_park_publication_serialization(
            previous_core.id(),
            false,
            true,
        );
        #[cfg(any(test, all(axtest, feature = "axtest"), feature = "task-test-hooks"))]
        park_after_final_wake_check_hook(self, previous_core.id());
        if previous_core
            .publish_blocked_from_parking()
            .unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x504b_1114, previous_core.id().as_u64() as usize)
            })
            == ParkPublication::Notified
        {
            drop(publication);
            transaction.commit_and_finish_scheduler_request();
            self.finish_owner_dispatch_commit(
                cpu.as_mut(),
                dispatch_commit,
                clock.wall().as_nanos(),
            );
            cpu.finish_park_preemption(true);
            token.mark_resolved();
            return Ok(Some(ParkCommit::Notified));
        }

        if previous_core.state() != ThreadState::Blocked
            || placement.queued_cpu() != Some(owner)
            || placement.on_cpu() != Some(owner)
        {
            task_runtime::fatal_invariant(0x504b_1115, previous_core.id().as_u64() as usize);
        }
        #[cfg(feature = "task-test-hooks")]
        park_after_blocked_publication_hook(self, previous_core.id());
        let active = transaction
            .deactivate_task(previous_core.id())
            .into_active();
        placement.block_current(owner);
        // Publish the detached owner only after `on_rq = NONE` and `on_cpu =
        // NONE`. A task-lock reader that acquired before the marker waits in
        // `DetachedActiveState::take`; a later reader waits before inspecting
        // task state. Neither can observe a Blocked RT task without its owner.
        publication.finish(active);
        #[cfg(feature = "task-test-hooks")]
        crate::task_test_hooks::record_park_profile_stage(4);
        cpu.finish_park_preemption(false);
        let outgoing = transaction.take_current().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x504b_1116, previous_core.id().as_u64() as usize)
        });
        if outgoing.thread() != previous_core.id() || outgoing.into_active().is_some() {
            task_runtime::fatal_invariant(0x504b_1117, previous_core.id().as_u64() as usize);
        }
        transaction.merge_scheduler_request(SchedulerRequestScope::All);
        #[cfg(feature = "task-test-hooks")]
        crate::task_test_hooks::record_park_profile_stage(5);
        let next = self.pick_owner_next_in_rq(cpu.as_mut(), &mut transaction, None);
        let next_core = next.core;
        let next_endpoint = transaction.current_switch_endpoint().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x504b_1118, next_core.id().as_u64() as usize)
        });
        Self::stage_switch_handoff(
            cpu.as_mut(),
            Some(token.thread()),
            Some(Arc::clone(previous_core)),
            Arc::clone(&next_core),
            None,
        );
        #[cfg(feature = "task-test-hooks")]
        crate::task_test_hooks::record_park_profile_stage(11);
        let deadline_rq_observation =
            transaction.scheduler_deadline_rq_observation(cpu.as_ref().get_ref());
        #[cfg(feature = "task-test-hooks")]
        crate::task_test_hooks::record_park_profile_stage(12);
        self.commit_owner_switch_selection(cpu.as_mut(), transaction, true);
        #[cfg(feature = "task-test-hooks")]
        crate::task_test_hooks::record_park_profile_stage(13);
        let decision = Self::owner_switch_plan(
            Some(previous_core),
            Some(previous_endpoint),
            &next_core,
            next_endpoint,
            SwitchReason::Blocked,
            now_ns,
        );
        self.finish_owner_dispatch_commit(cpu.as_mut(), dispatch_commit, clock.wall().as_nanos());
        let decision = self.finish_owner_selection(cpu.as_mut(), decision, deadline_rq_observation);
        #[cfg(feature = "task-test-hooks")]
        crate::task_test_hooks::record_park_profile_stage(14);
        token.mark_resolved();
        Ok(Some(ParkCommit::Blocked(decision)))
    }

    /// Cancels a prepared park because an independent grant won the race.
    pub fn cancel_park(
        &self,
        cpu: Pin<&mut CpuLocal>,
        current: &ThreadHandle,
        token: &mut ParkTicket,
    ) -> Result<(), TaskError> {
        self.cancel_current_park(cpu, current.runtime_core_arc(), token)
    }

    pub(crate) fn cancel_current_park(
        &self,
        cpu: Pin<&mut CpuLocal>,
        current: &Arc<ThreadCore>,
        token: &mut ParkTicket,
    ) -> Result<(), TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        if token.is_resolved() || current.id() != token.thread() {
            return Err(TaskError::StaleThreadId);
        }
        self.ensure_owner_cpu_online(&cpu)?;
        let core = Arc::clone(current);
        if core.park_generation() != token.generation() {
            return Err(TaskError::StaleThreadId);
        }
        let placement = core.sched().placement();
        if core.state() != ThreadState::Parking
            || placement.queued_cpu() != Some(cpu.owner())
            || placement.on_cpu() != Some(cpu.owner())
        {
            return Err(TaskError::StaleThreadId);
        }
        core.transition_state(ThreadState::Running)?;
        cpu.finish_park_preemption(true);
        token.mark_resolved();
        Ok(())
    }

    /// Validates all fallible current-thread exit prerequisites without
    /// publishing the thread as exited.
    pub(crate) fn prepare_current_exit(
        &self,
        cpu: Pin<&mut CpuLocal>,
        current: &ThreadHandle,
    ) -> Result<CurrentExitPermit, TaskError> {
        self.prepare_current_exit_inner(cpu, current, true)
    }

    pub(super) fn prepare_current_exit_inner(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        current: &ThreadHandle,
        require_runtime_context: bool,
    ) -> Result<CurrentExitPermit, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        self.complete_context_switch(cpu.as_mut())?;
        self.drain_owner_work(cpu.as_mut())?;
        let current_id = current.id();
        if cpu.remote().idle_thread() == Some(current_id) {
            return Err(TaskError::InvalidConfiguration);
        }
        let current_core = Arc::clone(current.runtime_core_arc());
        // Close before taking registry or thread-state locks. An activity that
        // won before this edge may need either lock to finish, just as Linux
        // takes p->pi_lock before rq/task-state validation rather than waiting
        // for a reader while holding rq.
        let scheduler_exit = current_core
            .close_owned_scheduler_activity()
            .ok_or(TaskError::ThreadBusy)?;
        let state = self.state.lock();
        state.ensure_cpu_online(&cpu)?;
        let record = state.thread_record(current_id)?;
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
        if placement.queued_cpu() != Some(cpu.owner()) || placement.on_cpu() != Some(cpu.owner()) {
            return Err(TaskError::ThreadBusy);
        }
        if require_runtime_context && record.resources.context().is_none() {
            return Err(TaskError::InvalidRuntimeHandle);
        }
        record.callbacks.validate_prepare_exit()?;
        Ok(CurrentExitPermit {
            scheduler_exit,
            current_core,
        })
    }

    /// Atomically prepares and commits current-thread exit.
    ///
    /// Runtime integrations that publish OS completion between those phases
    /// use the crate-private prepared form instead.
    pub fn exit_current(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        current: ThreadHandle,
    ) -> Result<ScheduleDecision, TaskError> {
        // Pure scheduler users may model a transition without installing an
        // architecture context. The runtime facade uses the stricter prepared
        // form before publishing OS-visible completion.
        let permit = self.prepare_current_exit_inner(cpu.as_mut(), &current, false)?;
        // The architecture current entry no longer needs a lookup lease once
        // the permit pins its core. Release it before publishing Exited so its
        // eventual lease drop cannot manufacture pre-switch-tail reap work.
        drop(current);
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
        // SAFETY: propagated from this method's scheduler-frame contract.
        unsafe { self.complete_context_switch_in_scheduler_frame(cpu.as_mut())? };
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
        let exited_core = Arc::clone(permit.current_core());
        {
            let state = self.state.lock();
            state.ensure_cpu_online(&cpu)?;
            let record = state.thread_record(exiting)?;
            if !Arc::ptr_eq(&record.core, &exited_core) {
                return Err(TaskError::StaleThreadId);
            }
            if record.has_live_pi_edges() {
                return Err(TaskError::InvalidPiState);
            }
            record.callbacks.validate_prepare_exit()?;
        }

        let remote = Arc::clone(cpu.remote());
        let initial_request = remote.claim_scheduler_request(SchedulerRequestScope::All);
        // SAFETY: propagated from the selected entry contract.
        let mut exited_sched = unsafe { rq_entry.lock_thread_sched(exited_core.sched()) };
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
            transaction.commit_and_finish_scheduler_request();
            return Err(TaskError::StaleThreadId);
        }
        transaction.adopt_scheduler_request(initial_request);
        transaction.merge_scheduler_request(SchedulerRequestScope::All);
        let dispatch_commit = self.settle_owner_current_dispatch_in_rq(&mut transaction);
        // Exit necessarily selects a replacement, so accounting requests from
        // the outgoing task are consumed by this decision.
        transaction.merge_scheduler_request(SchedulerRequestScope::All);
        let previous_endpoint = transaction.current_switch_endpoint().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x4558_0007, exiting.as_u64() as usize)
        });
        let held_reservation = {
            let placement = exited_core.sched().placement();
            let sched = &mut *exited_sched;
            if sched.lifecycle.state() != ThreadState::Running
                || placement.queued_cpu() != Some(cpu.owner())
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
        let next = self.pick_owner_next_in_rq(cpu.as_mut(), &mut transaction, None);
        let next_core = next.core;
        let next_endpoint = transaction.current_switch_endpoint().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x4558_0008, exiting.as_u64() as usize)
        });
        Self::stage_switch_handoff(
            cpu.as_mut(),
            Some(exiting),
            Some(Arc::clone(&exited_core)),
            Arc::clone(&next_core),
            None,
        );
        self.validate_owner_runtime_switch_out(cpu.as_ref().get_ref(), &transaction);
        let deadline_rq_observation =
            transaction.scheduler_deadline_rq_observation(cpu.as_ref().get_ref());
        transaction.commit_and_finish_scheduler_request();
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
        let decision = self.finish_owner_selection(cpu.as_mut(), decision, deadline_rq_observation);
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
        cpu: Pin<&mut CpuLocal>,
    ) -> Result<SwitchInCompletion, TaskError> {
        // SAFETY: the irqsave entry establishes its own IRQ ownership.
        unsafe { self.complete_context_switch_owner(cpu, OwnerRqEntry::IrqSave) }
    }

    /// Completes switch tail below the runtime's IRQ-off scheduler baton.
    ///
    /// # Safety
    ///
    /// The scheduler frame must remain active until this function returns.
    pub(crate) unsafe fn complete_context_switch_in_scheduler_frame(
        &self,
        cpu: Pin<&mut CpuLocal>,
    ) -> Result<SwitchInCompletion, TaskError> {
        // SAFETY: forwarded from this method's scheduler-frame contract.
        unsafe { self.complete_context_switch_owner(cpu, OwnerRqEntry::SchedulerFrame) }
    }

    /// Completes switch tail under the selected IRQ ownership protocol.
    ///
    /// # Safety
    ///
    /// `SchedulerFrame` requires an active IRQ-off runtime scheduler baton.
    pub(super) unsafe fn complete_context_switch_owner(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        rq_entry: OwnerRqEntry,
    ) -> Result<SwitchInCompletion, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        let Some(initial_handoff) = cpu.as_ref().get_ref().switch_handoff() else {
            return Ok(SwitchInCompletion::NONE);
        };
        let owner = cpu.owner();
        let previous_core = Arc::clone(initial_handoff.previous());
        let incoming = Arc::clone(initial_handoff.incoming());
        let migration_target = initial_handoff.migration_target();
        let runtime_tail_finished = initial_handoff.runtime_tail_is_finished();
        let rq_baton_retained = initial_handoff.has_rq_baton();
        if previous_core.id() == incoming.id()
            || previous_core.sched().placement().on_cpu() != Some(owner)
            || incoming.sched().placement().queued_cpu() != Some(owner)
            || incoming.sched().placement().on_cpu() != Some(owner)
            || (migration_target.is_some() && rq_baton_retained)
        {
            return Err(TaskError::InvalidConfiguration);
        }
        if !runtime_tail_finished {
            let reclaim_ready = task_runtime::finish_context_switch_tail();
            if cpu
                .as_mut()
                .finish_switch_runtime_tail(previous_core.id(), migration_target, reclaim_ready)
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
        if !Arc::ptr_eq(handoff.previous(), &previous_core)
            || !Arc::ptr_eq(handoff.incoming(), &incoming)
            || incoming.id() == previous
        {
            return Err(TaskError::InvalidConfiguration);
        }
        let (migration_target, previous_exited, affinity_completed) = if migration_target.is_some()
        {
            let placement = previous_core.sched().placement();
            #[cfg(feature = "task-test-hooks")]
            crate::task_test_hooks::record_switch_tail_thread_sched_acquisition(previous_core.id());
            // SAFETY: propagated from this method's selected entry contract.
            let mut sched = unsafe { rq_entry.lock_thread_sched(handoff.previous().sched()) };
            let remote = Arc::clone(cpu.remote());
            // SAFETY: propagated from this method's selected entry contract.
            let mut transaction = unsafe { rq_entry.begin(self, &remote) };
            #[cfg(feature = "task-test-hooks")]
            crate::task_test_hooks::record_switch_tail_irq_owner_scopes(
                previous_core.id(),
                sched.owns_runtime_irq_scope(),
                transaction.owns_runtime_irq_scope(),
                true,
                false,
            );
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
            (migration_target, previous_exited, affinity_completed)
        } else {
            // Linux `finish_task_switch()` runs `finish_task(prev)` — the
            // release-store of `prev->on_cpu` — while still holding
            // `rq->lock`, and only then `finish_lock_switch()` drops it.
            // Publishing the release inside the owner rq transaction keeps a
            // concurrent owner transaction (policy update classification and
            // re-link, wake, affinity reconcile) from observing `on_cpu`
            // flipping mid-transaction. Like Linux, ordinary switch tail does
            // not reopen `p->pi_lock`: remote affinity changes are serialized
            // through the rq owner's inbox, while current-task changes request
            // migration and rescheduling before reaching this tail.
            let previous_exited = if rq_baton_retained {
                let previous_exited = previous_core.state() == ThreadState::Exited;
                #[cfg(feature = "task-test-hooks")]
                crate::task_test_hooks::record_switch_tail_state_observation(
                    previous_core.id(),
                    previous_core.sched().placement().on_cpu() == Some(owner),
                );
                previous_core.sched().placement().finish_task(owner);
                if cpu.as_mut().finish_switch_rq_baton(previous_core.id()) != Ok(true) {
                    task_runtime::fatal_invariant(
                        0x5357_0005,
                        previous_core.id().as_u64() as usize,
                    );
                }
                previous_exited
            } else {
                let remote = Arc::clone(cpu.remote());
                // SAFETY: propagated from this method's selected entry contract.
                let transaction = unsafe { rq_entry.begin(self, &remote) };
                let previous_exited = previous_core.state() == ThreadState::Exited;
                #[cfg(feature = "task-test-hooks")]
                crate::task_test_hooks::record_switch_tail_state_observation(
                    previous_core.id(),
                    previous_core.sched().placement().on_cpu() == Some(owner),
                );
                previous_core.sched().placement().finish_task(owner);
                transaction.commit();
                previous_exited
            };
            #[cfg(feature = "task-test-hooks")]
            crate::task_test_hooks::record_switch_tail_irq_owner_scopes(
                previous_core.id(),
                false,
                false,
                !rq_baton_retained,
                rq_baton_retained,
            );
            (None, previous_exited, false)
        };
        #[cfg(feature = "task-test-hooks")]
        crate::task_test_hooks::complete_switch_tail_irq_owner_probe(previous_core.id());
        if affinity_completed {
            previous_core.notify_affinity_waiters();
        }
        let consumed = cpu.as_mut().take_switch_handoff().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x5357_0003, previous.as_u64() as usize)
        });
        if consumed.previous().id() != previous
            || consumed.incoming().id() != incoming.id()
            || consumed.migration_target() != migration_target
        {
            task_runtime::fatal_invariant(0x5357_0004, previous.as_u64() as usize);
        }
        let completed = consumed.into_runtime_finished().unwrap_or_else(|_| {
            task_runtime::fatal_invariant(0x5357_0004, previous.as_u64() as usize)
        });
        if !Arc::ptr_eq(&completed.previous, &previous_core)
            || !Arc::ptr_eq(&completed.incoming, &incoming)
        {
            task_runtime::fatal_invariant(0x5357_0004, previous.as_u64() as usize)
        }
        if let Some(migration) = completed.migration {
            migration.commit();
        }
        if completed.reclaim_ready {
            self.publish_resource_release_ready();
        }
        if previous_exited {
            self.task_work.publish();
        }
        let completion = SwitchInCompletion::for_core(&incoming);
        Ok(completion)
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
                if sched.lifecycle.state() != ThreadState::Running
                    || placement.queued_cpu().is_some()
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
