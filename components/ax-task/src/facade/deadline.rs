use super::*;

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
    thread: ThreadHandle,
    ticket: Option<crate::ParkTicket>,
}

/// Terminal scheduler information returned after a prepared park resumes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CurrentParkResume {
    generation: u64,
    deadline_expired: bool,
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
        self.thread.wake_handle()
    }

    /// Returns this park attempt's monotonically increasing generation.
    pub fn generation(&self) -> u64 {
        self.ticket()
            .expect("prepared park ticket remains owned")
            .generation()
    }

    /// Arms an absolute deadline in the runtime's finite monotonic domain.
    pub fn arm_deadline(&mut self, deadline: MonotonicDeadline) -> Result<(), TaskError> {
        let thread = self.thread.clone();
        let ticket = self
            .ticket
            .as_mut()
            .expect("prepared park ticket remains owned");
        arm_current_park_deadline(&thread, ticket, deadline)
    }

    /// Commits the scheduler park and returns after this thread is runnable again.
    pub fn commit(mut self) -> Result<CurrentParkResume, TaskError> {
        let mut ticket = self
            .ticket
            .take()
            .expect("prepared park ticket remains owned");
        let generation = ticket.generation();
        let deadline_armed = ticket.has_deadline();
        if let Err(error) = commit_current_park(&mut ticket) {
            let deadline_result = cancel_current_park_deadline(&self.thread, &mut ticket);
            if cancel_current_park(&mut ticket).is_err() {
                task_runtime::fatal_invariant(0x5041_0002, self.thread.id().as_u64() as usize);
            }
            let _cancelled = deadline_result?;
            return Err(error);
        }
        let deadline_cancelled = cancel_current_park_deadline(&self.thread, &mut ticket)?;
        Ok(CurrentParkResume {
            generation,
            deadline_expired: deadline_armed && !deadline_cancelled,
        })
    }

    /// Cancels this transaction without blocking the current thread.
    pub fn cancel(mut self) -> Result<(), TaskError> {
        let mut ticket = self
            .ticket
            .take()
            .expect("prepared park ticket remains owned");
        let deadline_result = cancel_current_park_deadline(&self.thread, &mut ticket);
        let park_result = cancel_current_park(&mut ticket);
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
    let mut irq = RuntimeIrqGuard::enter();
    let mut cpu = runtime_current_cpu_mut(&mut irq)?;
    let thread = cpu.current_thread_handle()?;
    match system.prepare_park(cpu.as_mut())? {
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
) -> Result<TaskClockEventOutcome, TaskError> {
    let system = runtime_task_system()?;
    let mut irq = RuntimeIrqGuard::enter();
    let mut cpu = runtime_current_cpu_mut(&mut irq)?;
    let (charge, clock, current) = system.charge_current_until_with_clock(cpu.as_mut(), 0)?;
    let rt_unthrottled = system.service_rt_period(&cpu, now);
    let mut hard_processed = 0;
    let mut hard_pending = false;
    while hard_processed < budget {
        let (event, pending) = cpu.as_mut().take_due_scheduler_deadline(now);
        hard_pending = pending;
        let Some(event) = event else {
            break;
        };
        system.service_expired_scheduler_deadline(cpu.as_mut(), event)?;
        hard_processed += 1;
    }
    hard_pending |= cpu.has_due_scheduler_deadline(now);
    if hard_pending {
        // The hard-IRQ budget is an execution bound, not a progress source.
        // Transfer the remainder to the owner scheduler safe point and keep a
        // sticky reschedule request so idle cannot wait for another timer IRQ.
        cpu.publish_hard_timer_work();
        cpu.request_reschedule();
    }
    let batch = cpu
        .as_mut()
        .on_task_clock_event(now, budget.saturating_sub(hard_processed));
    let scheduler_due = cpu.as_mut().scheduler_work_due(now);
    let pending = hard_pending || batch.pending() || scheduler_due || rt_unthrottled;
    let update = cpu.as_mut().next_scheduler_deadline_update(now)?;
    Ok(TaskClockEventOutcome {
        slice_expired: charge.slice_expired(),
        deadline_overrun: charge.deadline_overrun(),
        expired: hard_processed.saturating_add(batch.expired()),
        pending,
        update,
        scheduler_tick: SchedulerTickStamp {
            cpu: cpu.owner(),
            thread: current,
            observed_ns: clock.task().as_nanos(),
        },
    })
}

/// Publishes extension work for a periodic scheduler tick already accounted by
/// [`on_clock_event`].
///
/// The opaque stamp binds this publication to the exact `rq->curr` and task
/// clock sampled by the preceding owner-rq transaction. Physical clockevent
/// sources therefore do not pass compatibility booleans into task deadline
/// processing, and a delayed publication cannot silently target a new task.
pub fn publish_scheduler_tick(stamp: SchedulerTickStamp) -> Result<(), TaskError> {
    let system = runtime_task_system()?;
    let mut irq = RuntimeIrqGuard::enter();
    let cpu = runtime_current_cpu_mut(&mut irq)?;
    if cpu.owner() != stamp.cpu {
        return Err(TaskError::CpuOwnerMismatch {
            expected: stamp.cpu.as_u32(),
            actual: cpu.owner().as_u32(),
        });
    }
    system.publish_current_scheduler_tick_work(&cpu, stamp.thread, stamp.observed_ns)
}

#[cfg(test)]
pub(crate) fn prepare_current_park(_permit: &BlockingPermit) -> Result<ParkPrepare, TaskError> {
    let mut irq = RuntimeIrqGuard::enter();
    let system = runtime_task_system()?;
    let mut cpu = runtime_current_cpu_mut(&mut irq)?;
    system.prepare_park(cpu.as_mut())
}

pub(crate) fn commit_current_park(ticket: &mut crate::ParkTicket) -> Result<(), TaskError> {
    validate_blocking_context()?;
    let mut scheduler_frame = RuntimeSchedulerFrameGuard::enter(
        RuntimeScheduleOrigin::Block,
        RuntimeSchedulerEntry::Task,
    )?;
    let system = runtime_task_system()?;
    let commit = {
        let mut cpu = runtime_current_cpu_mut(&mut scheduler_frame)?;
        // SAFETY: `scheduler_frame` owns the IRQ-off scheduler baton.
        unsafe { system.commit_park_in_scheduler_frame(cpu.as_mut(), ticket)? }
    };
    match commit {
        ParkCommit::Notified => Ok(()),
        ParkCommit::Blocked(decision) => {
            execute_switch_plan(&mut scheduler_frame, decision);
            Ok(())
        }
    }
}

pub(crate) fn cancel_current_park(ticket: &mut crate::ParkTicket) -> Result<(), TaskError> {
    let mut irq = RuntimeIrqGuard::enter();
    let mut cpu = runtime_current_cpu_mut(&mut irq)?;
    runtime_task_system()?.cancel_park(cpu.as_mut(), ticket)
}

pub(crate) fn arm_current_park_deadline(
    thread: &ThreadHandle,
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
    let (registration, update) = {
        let owner = cpu.owner();
        let registration = cpu
            .lock_deadline_base()
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
        thread.core.register_sleep_timer(owner, token.generation());
        let monotonic_now = task_runtime::monotonic_now();
        let update = match cpu.as_mut().next_scheduler_deadline_update(monotonic_now) {
            Ok(update) => update,
            Err(error) => {
                let removed = cpu.lock_deadline_base().queue.cancel(&registration);
                let completed = thread.core.complete_sleep_timer(token.generation());
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
    thread: &ThreadHandle,
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
    let Some(expected) = thread.core.sleep_timer_cpu_for(token.generation()) else {
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
            let mut deadline_base = remote.lock_deadline_base();
            let cancellation = deadline_base.queue.begin_cancel(registration);
            let expired = if cancellation.is_none() {
                deadline_base
                    .take_buffered_expiration(registration)
                    .is_some()
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
            (None, false)
                if thread
                    .core
                    .sleep_timer_cpu_for(token.generation())
                    .is_none() =>
            {
                if !ticket.clear_deadline(token) {
                    task_runtime::fatal_invariant(0x5444_0003, thread.id().as_u64() as usize);
                }
                return Ok(false);
            }
            (None, false) => {
                task_runtime::fatal_invariant(0x5444_0006, thread.id().as_u64() as usize)
            }
        };
        if !thread.core.complete_sleep_timer(token.generation()) || !ticket.clear_deadline(token) {
            task_runtime::fatal_invariant(0x5444_0004, thread.id().as_u64() as usize);
        }
        return Ok(cancelled);
    }
    let (cancellation, update) = {
        let registration = ticket
            .deadline()
            .expect("the deadline registration remains owned until cancellation");
        let cancellation_state = {
            let mut deadline_base = cpu.lock_deadline_base();
            let cancellation = deadline_base.queue.begin_cancel(registration);
            let expired = if cancellation.is_none() {
                deadline_base
                    .take_buffered_expiration(registration)
                    .is_some()
            } else {
                false
            };
            (cancellation, expired)
        };
        let cancellation = match cancellation_state {
            (Some(cancellation), _) => cancellation,
            (None, true) => {
                if !thread.core.complete_sleep_timer(token.generation())
                    || !ticket.clear_deadline(token)
                {
                    task_runtime::fatal_invariant(0x5444_0004, thread.id().as_u64() as usize);
                }
                return Ok(false);
            }
            (None, false)
                if thread
                    .core
                    .sleep_timer_cpu_for(token.generation())
                    .is_none() =>
            {
                if !ticket.clear_deadline(token) {
                    task_runtime::fatal_invariant(0x5444_0003, thread.id().as_u64() as usize);
                }
                return Ok(false);
            }
            (None, false) => {
                task_runtime::fatal_invariant(0x5444_0006, thread.id().as_u64() as usize);
            }
        };
        let monotonic_now = task_runtime::monotonic_now();
        let update = match cpu.as_mut().next_scheduler_deadline_update(monotonic_now) {
            Ok(update) => update,
            Err(error) => {
                cancellation.rollback(&mut cpu.lock_deadline_base().queue);
                cpu.as_mut().invalidate_scheduler_deadline_publication();
                return Err(error);
            }
        };
        (cancellation, update)
    };
    task_runtime::publish_scheduler_deadline(update);
    cancellation.commit();
    if !thread.core.complete_sleep_timer(token.generation()) || !ticket.clear_deadline(token) {
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
    pending: bool,
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
    /// Returns whether another bounded expiry pass is immediately required.
    pub const fn pending(self) -> bool {
        self.pending
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
