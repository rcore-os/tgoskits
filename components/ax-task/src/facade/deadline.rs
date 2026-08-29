use super::*;
use crate::{DeadlineBaseGuardSource, SchedulerDeadlineDerivationSource};

/// Result of beginning an externally queued current-thread park transaction.
///
/// OS wait subsystems use this after validating their condition under their own
/// queue lock. The scheduler still owns the generation, deadline, and context
/// switch transaction; callers own only publication and removal of their
/// domain-specific waiter record.
#[derive(Debug)]
pub enum CurrentParkStart {
    /// A preceding wake was consumed, so no waiter may be published for this attempt.
    Notified,
    /// The current thread entered `Parking` and must be committed or cancelled.
    Prepared(PreparedCurrentPark),
}

/// Move-only ownership of one prepared current-thread park transaction.
#[must_use = "a prepared current-thread park must be committed or cancelled"]
#[derive(Debug)]
pub struct PreparedCurrentPark {
    thread: Arc<ThreadCore>,
    ticket: Option<crate::ParkTicket>,
}

/// Terminal scheduler information returned after a prepared park resumes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CurrentParkResume {
    generation: u64,
    deadline_expired: bool,
    disposition: CurrentParkDisposition,
}

/// Scheduler disposition of one completed current-thread park transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CurrentParkDisposition {
    /// A scheduler notification cancelled the park before schedule-out.
    NotifiedBeforeBlock,
    /// The current thread committed `Blocked`, switched out, and later resumed.
    BlockedAndResumed,
}

impl CurrentParkResume {
    /// Returns the scheduler park generation completed by this transaction.
    pub const fn generation(self) -> u64 {
        self.generation
    }

    /// Reports whether an armed task deadline physically expired before cleanup.
    pub const fn deadline_expired(self) -> bool {
        self.deadline_expired
    }

    /// Returns whether this park switched out or was cancelled before blocking.
    pub const fn disposition(self) -> CurrentParkDisposition {
        self.disposition
    }

    /// Reports whether a scheduler notification cancelled schedule-out.
    pub const fn was_notified_before_block(self) -> bool {
        matches!(
            self.disposition,
            CurrentParkDisposition::NotifiedBeforeBlock
        )
    }
}

impl PreparedCurrentPark {
    /// Returns the generation-bearing scheduler identity being parked.
    pub fn thread_id(&self) -> ThreadId {
        self.thread.id()
    }

    /// Returns a generation-bearing wake capability for this parked thread.
    ///
    /// External waiter queues should publish only this restricted capability,
    /// not a full scheduler thread handle.
    pub fn wake_handle(&self) -> ThreadWakeHandle {
        ThreadWakeHandle::from_core(Arc::clone(&self.thread))
    }

    /// Returns this park attempt's monotonically increasing generation.
    pub fn generation(&self) -> u64 {
        self.ticket()
            .expect("prepared park ticket remains owned")
            .generation()
    }

    /// Arms an absolute deadline in the runtime's finite monotonic domain.
    pub fn arm_deadline(&mut self, deadline: MonotonicDeadline) -> Result<(), TaskError> {
        let thread = self.thread.as_ref();
        let ticket = self
            .ticket
            .as_mut()
            .expect("prepared park ticket remains owned");
        arm_current_park_deadline(thread, ticket, deadline)
    }

    /// Commits the scheduler park and returns after this thread is runnable again.
    pub fn commit(mut self) -> Result<CurrentParkResume, TaskError> {
        let mut ticket = self
            .ticket
            .take()
            .expect("prepared park ticket remains owned");
        let generation = ticket.generation();
        let deadline_armed = ticket.has_deadline();
        let disposition = match commit_current_park(&self.thread, &mut ticket) {
            Ok(disposition) => disposition,
            Err(error) => {
                let deadline_result = cancel_current_park_deadline(&self.thread, &mut ticket);
                if cancel_current_park(&self.thread, &mut ticket).is_err() {
                    task_runtime::fatal_invariant(0x5041_0002, self.thread.id().as_u64() as usize);
                }
                let _cancelled = deadline_result?;
                return Err(error);
            }
        };
        let deadline_cancelled = cancel_current_park_deadline(&self.thread, &mut ticket)?;
        Ok(CurrentParkResume {
            generation,
            deadline_expired: deadline_armed && !deadline_cancelled,
            disposition,
        })
    }

    /// Cancels this transaction without blocking the current thread.
    pub fn cancel(mut self) -> Result<(), TaskError> {
        let mut ticket = self
            .ticket
            .take()
            .expect("prepared park ticket remains owned");
        let deadline_result = cancel_current_park_deadline(&self.thread, &mut ticket);
        let park_result = cancel_current_park(&self.thread, &mut ticket);
        let _cancelled = deadline_result?;
        park_result
    }

    fn ticket(&self) -> Option<&crate::ParkTicket> {
        self.ticket.as_ref()
    }
}

impl Drop for PreparedCurrentPark {
    fn drop(&mut self) {
        if self
            .ticket
            .as_ref()
            .is_some_and(|ticket| !ticket.is_resolved())
        {
            task_runtime::fatal_invariant(0x5041_0003, self.thread.id().as_u64() as usize);
        }
    }
}

/// Begins a scheduler-owned park transaction for an OS-owned waiter queue.
///
/// The caller must serialize its condition check and waiter publication so a
/// selecting producer either observes the waiter or leaves the scheduler's
/// sticky wake-before-park notification. This function is bounded and does not
/// sleep, allocate, or invoke OS callbacks.
pub fn begin_current_park() -> Result<CurrentParkStart, TaskError> {
    let permit = acquire_blocking_permit()?;
    begin_current_park_with_permit(&permit)
}

pub(crate) fn begin_current_park_with_permit(
    _permit: &BlockingPermit,
) -> Result<CurrentParkStart, TaskError> {
    let system = runtime_task_system()?;
    // `current` is migration-stable only while task preemption is disabled.
    // Keep this lighter than the CPU/rq owner protocol: the pin exists solely
    // to make the independent task_cpu/on_rq and on_cpu publications one
    // current-task observation before PARKING becomes visible.
    let _current_pin = PreemptScope::enter();
    let thread = current_thread_core_arc()?;
    let prepare = system.prepare_current_park(&thread);
    match prepare? {
        ParkPrepare::Notified => Ok(CurrentParkStart::Notified),
        ParkPrepare::Prepared(ticket) => Ok(CurrentParkStart::Prepared(PreparedCurrentPark {
            thread,
            ticket: Some(ticket),
        })),
    }
}

/// Performs one bounded task-clockevent pass without allocation or callbacks.
pub fn on_clock_event(
    now: MonotonicInstant,
    budget: usize,
    scheduler_event: ClaimedSchedulerDeadlines,
) -> Result<TaskClockEventOutcome, TaskError> {
    let system = runtime_task_system()?;
    let mut irq = RuntimeIrqGuard::enter();
    let mut cpu = runtime_current_cpu_mut(&mut irq)?;
    if scheduler_event.runs_periodic_task_tick() {
        // Linux PREEMPT_RT promotes TIF_NEED_RESCHED_LAZY before invoking the
        // current class's periodic task_tick hook. A new lazy request created
        // by that hook remains lazy until the following promotion point.
        cpu.promote_lazy_reschedule();
    }
    let (charge, clock, current, task_tick_rq_observation) = match scheduler_event.accounting_kind()
    {
        ClockAccountingKind::RuntimeOnly => {
            system.charge_current_until_with_clock(cpu.as_mut(), 0)?
        }
        ClockAccountingKind::SchedulerDeadline => {
            system.clock_event_current_until_with_clock(cpu.as_mut(), 0)?
        }
        ClockAccountingKind::PeriodicTick => {
            system.task_tick_current_until_with_clock(cpu.as_mut(), 0)?
        }
        ClockAccountingKind::PeriodicTickWithSchedulerDeadline => {
            system.task_tick_and_clock_event_current_until_with_clock(cpu.as_mut(), 0)?
        }
    };
    let rt_period_rescheduled = system.service_rt_period(&cpu, now);
    let hard = system.service_due_hard_timers(cpu.as_mut(), now)?;
    let batch = cpu.as_mut().on_task_clock_event(now, budget);
    let rq_observation =
        if clock_event_rq_observation_reusable(rt_period_rescheduled, hard.processed()) {
            cpu.as_mut()
                .scheduler_work_due_from_rq_observation(now, task_tick_rq_observation)
        } else {
            cpu.as_mut().scheduler_work_due(now)
        };
    let update = cpu
        .as_mut()
        .next_scheduler_deadline_update_from_rq_observation(
            now,
            rq_observation,
            SchedulerDeadlineDerivationSource::ClockEvent,
        )?;
    Ok(TaskClockEventOutcome {
        slice_expired: charge.slice_expired(),
        deadline_overrun: charge.deadline_overrun(),
        expired: hard.processed().saturating_add(batch.expired()),
        update,
        scheduler_tick: SchedulerTickStamp {
            cpu: cpu.owner(),
            thread: current,
            observed_ns: clock.task().as_nanos(),
        },
    })
}

fn clock_event_rq_observation_reusable(
    rt_period_rescheduled: bool,
    hard_timers_processed: usize,
) -> bool {
    !rt_period_rescheduled && hard_timers_processed == 0
}

/// Scheduler-owned deadlines claimed by one physical clockevent firing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClaimedSchedulerDeadlines {
    periodic_tick_elapsed: bool,
    scheduler_deadline_elapsed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ClockAccountingKind {
    RuntimeOnly,
    SchedulerDeadline,
    PeriodicTick,
    PeriodicTickWithSchedulerDeadline,
}

impl ClaimedSchedulerDeadlines {
    /// Captures the two independent scheduler deadlines observed by the
    /// physical clockevent owner.
    pub const fn new(periodic_tick_elapsed: bool, scheduler_deadline_elapsed: bool) -> Self {
        Self {
            periodic_tick_elapsed,
            scheduler_deadline_elapsed,
        }
    }

    const fn runs_periodic_task_tick(self) -> bool {
        self.periodic_tick_elapsed
    }

    const fn accounting_kind(self) -> ClockAccountingKind {
        match (self.periodic_tick_elapsed, self.scheduler_deadline_elapsed) {
            (false, false) => ClockAccountingKind::RuntimeOnly,
            (false, true) => ClockAccountingKind::SchedulerDeadline,
            (true, false) => ClockAccountingKind::PeriodicTick,
            (true, true) => ClockAccountingKind::PeriodicTickWithSchedulerDeadline,
        }
    }
}

/// Samples CPU time and publishes extension work for a periodic scheduler tick
/// already accounted by [`on_clock_event`].
///
/// The opaque stamp binds this publication to the exact `rq->curr` and task
/// clock sampled by the preceding owner-rq transaction. Physical clockevent
/// sources therefore do not pass compatibility booleans into task deadline
/// processing, and a delayed publication cannot silently target a new task.
pub fn publish_scheduler_tick(stamp: SchedulerTickStamp, tick_ns: u64) -> Result<(), TaskError> {
    if tick_ns == 0 {
        return Err(TaskError::InvalidConfiguration);
    }
    let system = runtime_task_system()?;
    let mut irq = RuntimeIrqGuard::enter();
    let cpu = runtime_current_cpu_mut(&mut irq)?;
    if cpu.owner() != stamp.cpu {
        return Err(TaskError::CpuOwnerMismatch {
            expected: stamp.cpu.as_u32(),
            actual: cpu.owner().as_u32(),
        });
    }
    system.publish_current_scheduler_tick_work(&cpu, stamp.thread, stamp.observed_ns, tick_ns)
}

pub(crate) fn commit_current_park(
    current: &Arc<ThreadCore>,
    ticket: &mut crate::ParkTicket,
) -> Result<CurrentParkDisposition, TaskError> {
    validate_blocking_context()?;
    let mut scheduler_frame = RuntimeSchedulerFrameGuard::enter(
        RuntimeScheduleOrigin::Block,
        RuntimeSchedulerEntry::Task,
    )?;
    let system = runtime_task_system()?;
    let commit = {
        let mut cpu = runtime_current_cpu_mut(&mut scheduler_frame)?;
        // SAFETY: `scheduler_frame` owns the IRQ-off scheduler baton.
        unsafe { system.commit_park_in_scheduler_frame(cpu.as_mut(), current, ticket)? }
    };
    match commit {
        ParkCommit::Notified => Ok(CurrentParkDisposition::NotifiedBeforeBlock),
        ParkCommit::Blocked(decision) => {
            execute_switch_plan(&mut scheduler_frame, decision);
            Ok(CurrentParkDisposition::BlockedAndResumed)
        }
    }
}

pub(crate) fn cancel_current_park(
    current: &ThreadCore,
    ticket: &mut crate::ParkTicket,
) -> Result<(), TaskError> {
    let mut irq = RuntimeIrqGuard::enter();
    let mut cpu = runtime_current_cpu_mut(&mut irq)?;
    runtime_task_system()?.cancel_current_park(cpu.as_mut(), current, ticket)
}

pub(crate) fn arm_current_park_deadline(
    thread: &ThreadCore,
    ticket: &mut crate::ParkTicket,
    deadline: MonotonicDeadline,
) -> Result<(), TaskError> {
    let mut irq = RuntimeIrqGuard::enter();
    let mut cpu = runtime_current_cpu_mut(&mut irq)?;
    if ticket.thread() != thread.id()
        || ticket.is_resolved()
        || ticket.has_deadline()
        || cpu.current() != Some(thread.id())
    {
        return Err(TaskError::StaleThreadId);
    }
    let owner = cpu.owner();
    let monotonic_now = task_runtime::monotonic_now();
    let non_timer = cpu
        .as_mut()
        .prepare_scheduler_deadline_registration_publication(
            monotonic_now,
            SchedulerDeadlineDerivationSource::ParkArm,
        );
    let (registration, update) = {
        let mut deadline_base = cpu
            .remote()
            .lock_deadline_activity(DeadlineBaseGuardSource::Registration);
        let registration = deadline_base
            .queue
            .arm(
                thread.sleep_timer(),
                deadline,
                TaskDeadlineKind::park_timeout(ticket.generation()),
            )
            .map_err(|error| match error {
                crate::timer::TaskDeadlineError::Capacity => TaskError::TimerCapacity,
                crate::timer::TaskDeadlineError::GenerationExhausted
                | crate::timer::TaskDeadlineError::KindMismatch => TaskError::InvalidConfiguration,
            })?;
        let token = registration.token();
        thread.register_sleep_timer(owner, token.generation());
        let update = match CpuLocal::update_scheduler_deadline_registration_publication(
            &mut deadline_base,
            non_timer,
        ) {
            Ok(update) => update,
            Err(error) => {
                let removed = deadline_base.queue.cancel(&registration);
                let completed = thread.complete_sleep_timer(token.generation());
                if !removed || !completed {
                    task_runtime::fatal_invariant(0x5444_0005, thread.id().as_u64() as usize);
                }
                return Err(error);
            }
        };
        (registration, update)
    };
    task_runtime::publish_scheduler_deadline(update);
    if ticket.attach_deadline(registration).is_err() {
        task_runtime::fatal_invariant(0x5444_0002, thread.id().as_u64() as usize);
    }
    Ok(())
}

pub(crate) fn cancel_current_park_deadline(
    thread: &ThreadCore,
    ticket: &mut crate::ParkTicket,
) -> Result<bool, TaskError> {
    if ticket.thread() != thread.id() {
        return Err(TaskError::StaleThreadId);
    }
    let Some(token) = ticket.deadline().map(|registration| registration.token()) else {
        return Ok(false);
    };
    let system = runtime_task_system()?;
    let mut irq = RuntimeIrqGuard::enter();
    let mut cpu = runtime_current_cpu_mut(&mut irq)?;
    let actual = cpu.owner();
    let Some(expected) = thread.sleep_timer_cpu_for(token.generation()) else {
        // Expiration physically removes the queue entry and clears the core's
        // matching generation before the owner thread resumes. Only that
        // terminal state permits consuming the ticket without queue access.
        if !ticket.clear_deadline(token) {
            task_runtime::fatal_invariant(0x5444_0003, thread.id().as_u64() as usize);
        }
        return Ok(false);
    };
    if actual != expected {
        let remote = system
            .cpu_remote(expected)
            .ok_or(TaskError::CpuOffline(expected.as_u32()))?;
        let registration = ticket
            .deadline()
            .expect("the deadline registration remains owned until cancellation");
        let (cancellation, expired) = {
            let mut deadline_base =
                remote.lock_deadline_activity(DeadlineBaseGuardSource::Registration);
            let cancellation = deadline_base.queue.begin_cancel(registration);
            let expired = if cancellation.is_none() {
                deadline_base.cancel_expired_task_deadline(registration)
            } else {
                false
            };
            (cancellation, expired)
        };
        let cancelled = match (cancellation, expired) {
            (Some(cancellation), _) => {
                // Linux does not reprogram another CPU's clockevent when a
                // remote hrtimer is removed. The stale edge is conservative;
                // its owner recomputes the authoritative queue when it fires.
                cancellation.commit();
                true
            }
            (None, true) => false,
            (None, false) if thread.sleep_timer_cpu_for(token.generation()).is_none() => {
                if !ticket.clear_deadline(token) {
                    task_runtime::fatal_invariant(0x5444_0003, thread.id().as_u64() as usize);
                }
                return Ok(false);
            }
            (None, false) => {
                task_runtime::fatal_invariant(0x5444_0006, thread.id().as_u64() as usize)
            }
        };
        if !thread.complete_sleep_timer(token.generation()) || !ticket.clear_deadline(token) {
            task_runtime::fatal_invariant(0x5444_0004, thread.id().as_u64() as usize);
        }
        return Ok(cancelled);
    }
    let monotonic_now = task_runtime::monotonic_now();
    let non_timer = cpu
        .as_mut()
        .prepare_scheduler_deadline_registration_publication(
            monotonic_now,
            SchedulerDeadlineDerivationSource::ParkCancel,
        );
    let (cancellation, update) = {
        let registration = ticket
            .deadline()
            .expect("the deadline registration remains owned until cancellation");
        let mut deadline_base = cpu
            .remote()
            .lock_deadline_activity(DeadlineBaseGuardSource::Registration);
        let cancellation = deadline_base.queue.begin_cancel(registration);
        let expired = if cancellation.is_none() {
            deadline_base.cancel_expired_task_deadline(registration)
        } else {
            false
        };
        let cancellation = match (cancellation, expired) {
            (Some(cancellation), _) => cancellation,
            (None, true) => {
                if !thread.complete_sleep_timer(token.generation()) || !ticket.clear_deadline(token)
                {
                    task_runtime::fatal_invariant(0x5444_0004, thread.id().as_u64() as usize);
                }
                return Ok(false);
            }
            (None, false) if thread.sleep_timer_cpu_for(token.generation()).is_none() => {
                if !ticket.clear_deadline(token) {
                    task_runtime::fatal_invariant(0x5444_0003, thread.id().as_u64() as usize);
                }
                return Ok(false);
            }
            (None, false) => {
                task_runtime::fatal_invariant(0x5444_0006, thread.id().as_u64() as usize);
            }
        };
        let update = match CpuLocal::update_scheduler_deadline_registration_publication(
            &mut deadline_base,
            non_timer,
        ) {
            Ok(update) => update,
            Err(error) => {
                cancellation.rollback(&mut deadline_base.queue);
                return Err(error);
            }
        };
        (cancellation, update)
    };
    task_runtime::publish_scheduler_deadline(update);
    cancellation.commit();
    if !thread.complete_sleep_timer(token.generation()) || !ticket.clear_deadline(token) {
        task_runtime::fatal_invariant(0x5444_0004, thread.id().as_u64() as usize);
    }
    Ok(true)
}

/// Bounded task-clockevent result consumed by the runtime clockevent owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TaskClockEventOutcome {
    slice_expired: bool,
    deadline_overrun: bool,
    expired: usize,
    update: crate::runtime::SchedulerDeadlineUpdate,
    scheduler_tick: SchedulerTickStamp,
}

/// Opaque owner-rq sample required to publish one periodic scheduler tick.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SchedulerTickStamp {
    cpu: CpuId,
    thread: ThreadId,
    observed_ns: u64,
}

impl TaskClockEventOutcome {
    /// Returns whether the current scheduling slice or budget expired.
    pub const fn slice_expired(self) -> bool {
        self.slice_expired
    }
    /// Returns whether the current Deadline reservation exhausted its CBS budget.
    pub const fn deadline_overrun(self) -> bool {
        self.deadline_overrun
    }
    /// Returns the number of timer events claimed by this bounded IRQ pass.
    pub const fn expired(self) -> usize {
        self.expired
    }
    /// Returns the complete generation-ordered task-deadline publication.
    pub const fn update(self) -> crate::runtime::SchedulerDeadlineUpdate {
        self.update
    }
    /// Returns the rq-bound stamp consumed when this physical edge was also a
    /// periodic scheduler tick.
    pub const fn scheduler_tick_stamp(self) -> SchedulerTickStamp {
        self.scheduler_tick
    }
    /// Returns the next finite task-owned deadline.
    pub const fn next_deadline(self) -> Option<MonotonicDeadline> {
        self.update.deadline()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ClaimedSchedulerDeadlines, ClockAccountingKind, clock_event_rq_observation_reusable,
    };

    #[test]
    fn only_periodic_clock_events_run_the_scheduler_tick() {
        assert!(!ClaimedSchedulerDeadlines::new(false, false).runs_periodic_task_tick());
        assert!(!ClaimedSchedulerDeadlines::new(false, true).runs_periodic_task_tick());
        assert!(ClaimedSchedulerDeadlines::new(true, false).runs_periodic_task_tick());
        assert!(ClaimedSchedulerDeadlines::new(true, true).runs_periodic_task_tick());
    }

    #[test]
    fn unrelated_physical_clockevent_only_accounts_runtime() {
        assert_eq!(
            ClaimedSchedulerDeadlines::new(false, false).accounting_kind(),
            ClockAccountingKind::RuntimeOnly
        );
        assert_eq!(
            ClaimedSchedulerDeadlines::new(false, true).accounting_kind(),
            ClockAccountingKind::SchedulerDeadline
        );
        assert_eq!(
            ClaimedSchedulerDeadlines::new(true, false).accounting_kind(),
            ClockAccountingKind::PeriodicTick
        );
        assert_eq!(
            ClaimedSchedulerDeadlines::new(true, true).accounting_kind(),
            ClockAccountingKind::PeriodicTickWithSchedulerDeadline
        );
    }

    #[test]
    fn linux_common_tick_reuses_the_task_tick_rq_observation() {
        assert!(clock_event_rq_observation_reusable(false, 0));
        assert!(!clock_event_rq_observation_reusable(true, 0));
        assert!(!clock_event_rq_observation_reusable(false, 1));
    }
}
