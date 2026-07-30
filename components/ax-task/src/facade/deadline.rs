use super::*;

/// Performs one bounded task-clockevent pass without allocation or callbacks.
pub fn on_clock_event(now_ns: u64, budget: usize) -> Result<TaskClockEventOutcome, TaskError> {
    on_clock_event_with_scheduler_tick(now_ns, budget, false)
}

/// Performs one bounded task-clockevent pass and records a periodic scheduler tick.
///
/// Scheduler-tick extension work, when enabled, is deferred to the dedicated
/// task-work service and never invoked from this hard-IRQ path.
pub fn on_clock_event_with_scheduler_tick(
    now_ns: u64,
    budget: usize,
    scheduler_tick: bool,
) -> Result<TaskClockEventOutcome, TaskError> {
    let system = runtime_task_system()?;
    let mut irq = RuntimeIrqGuard::enter();
    let timer_resolution_ns = task_runtime::timer_resolution_ns();
    let mut cpu = runtime_current_cpu_mut(&mut irq)?;
    let charge = system.charge_current_until(cpu.as_mut(), now_ns, 0)?;
    if scheduler_tick {
        system.publish_current_scheduler_tick_work(&cpu, now_ns);
    }
    let batch = cpu
        .as_mut()
        .expire_task_deadlines(now_ns, timer_resolution_ns, budget);
    let scheduler_due = cpu.take_due_scheduler_deadline(now_ns);
    let pending = batch.pending() || scheduler_due;
    if charge.slice_expired() || charge.deadline_overrun() || batch.expired() != 0 || pending {
        cpu.request_reschedule();
    }
    let update = cpu
        .as_mut()
        .next_task_deadline_update(now_ns, timer_resolution_ns)?;
    Ok(TaskClockEventOutcome {
        slice_expired: charge.slice_expired(),
        deadline_overrun: charge.deadline_overrun(),
        expired: batch.expired(),
        pending,
        update,
    })
}

/// Copies the last IRQ's expired timer events for task-context processing.
pub fn take_current_expired_task_deadlines(
    output: &mut [ExpiredTaskDeadline],
) -> Result<usize, TaskError> {
    if task_runtime::in_hard_irq() {
        return Err(TaskError::UnsafeContext);
    }
    let mut irq = RuntimeIrqGuard::enter();
    let mut cpu = runtime_current_cpu_mut(&mut irq)?;
    Ok(cpu.as_mut().take_expired_task_deadlines(output))
}

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
    let deadline_now_ns = task_runtime::monotonic_ns();
    service_current_task_deadline_work(system, &mut scheduler_frame, deadline_now_ns)?;
    let now_ns = task_runtime::monotonic_ns();
    let commit = {
        let mut cpu = runtime_current_cpu_mut(&mut scheduler_frame)?;
        system.commit_park(cpu.as_mut(), ticket)?
    };
    match commit {
        ParkCommit::Notified => Ok(()),
        ParkCommit::Blocked(decision) => {
            execute_switch_plan(&mut scheduler_frame, decision, now_ns);
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
    deadline_ns: u64,
) -> Result<(), TaskError> {
    let mut irq = RuntimeIrqGuard::enter();
    if ticket.thread() != thread.id()
        || ticket.is_resolved()
        || ticket.has_deadline()
        || runtime_current_cpu()?.current() != Some(thread.id())
    {
        return Err(TaskError::StaleThreadId);
    }
    if deadline_ns == u64::MAX {
        // Saturated relative waits have no representable finite expiry. Keep
        // them as ordinary notification-only parks instead of consuming a
        // queue slot that the physical clockevent can never arm.
        return Ok(());
    }
    let now_ns = task_runtime::monotonic_ns();
    let resolution_ns = task_runtime::timer_resolution_ns();
    let (registration, update) = {
        let mut cpu = runtime_current_cpu_mut(&mut irq)?;
        let owner = cpu.owner();
        let registration = cpu
            .as_mut()
            .task_deadlines()
            .arm(
                thread.sleep_timer(),
                deadline_ns,
                TaskDeadlineKind::park_timeout(ticket.generation()),
            )
            .map_err(|error| match error {
                crate::timer::TaskDeadlineError::Capacity => TaskError::TimerCapacity,
                crate::timer::TaskDeadlineError::InvalidDeadline
                | crate::timer::TaskDeadlineError::GenerationExhausted
                | crate::timer::TaskDeadlineError::KindMismatch => TaskError::InvalidConfiguration,
            })?;
        let token = registration.token();
        thread.core.register_sleep_timer(owner, token.generation());
        let update = match cpu
            .as_mut()
            .next_task_deadline_update(now_ns, resolution_ns)
        {
            Ok(update) => update,
            Err(error) => {
                let removed = cpu.as_mut().task_deadlines().cancel(&registration);
                let completed = thread.core.complete_sleep_timer(token.generation());
                if !removed || !completed {
                    task_runtime::fatal_invariant(0x5444_0005, thread.id().as_u64() as usize);
                }
                return Err(error);
            }
        };
        (registration, update)
    };
    let status = task_runtime::publish_task_deadline(update);
    if status != crate::runtime::RuntimeStatus::Success {
        let token = registration.token();
        let rollback = {
            let mut cpu = runtime_current_cpu_mut(&mut irq)?;
            let _removed = cpu.as_mut().task_deadlines().cancel(&registration);
            thread.core.complete_sleep_timer(token.generation());
            cpu.as_mut()
                .next_task_deadline_update(now_ns, resolution_ns)?
        };
        let rollback_status = task_runtime::publish_task_deadline(rollback);
        if rollback_status != crate::runtime::RuntimeStatus::Success {
            task_runtime::fatal_invariant(0x5444_0001, rollback_status as usize);
        }
        return Err(TaskError::RuntimeFailure(status as u32));
    }
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
    let mut irq = RuntimeIrqGuard::enter();
    let actual = runtime_current_cpu()?.owner();
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
        return Err(TaskError::CpuOwnerMismatch {
            expected: expected.as_u32(),
            actual: actual.as_u32(),
        });
    }
    let now_ns = task_runtime::monotonic_ns();
    let resolution_ns = task_runtime::timer_resolution_ns();
    let (removed, update) = {
        let mut cpu = runtime_current_cpu_mut(&mut irq)?;
        let removed = cpu.as_mut().task_deadlines().cancel(
            ticket
                .deadline()
                .expect("the deadline registration remains owned until cancellation"),
        );
        thread.core.complete_sleep_timer(token.generation());
        if !ticket.clear_deadline(token) {
            task_runtime::fatal_invariant(0x5444_0004, thread.id().as_u64() as usize);
        }
        let update = cpu
            .as_mut()
            .next_task_deadline_update(now_ns, resolution_ns)?;
        (removed, update)
    };
    let status = task_runtime::publish_task_deadline(update);
    if status != crate::runtime::RuntimeStatus::Success {
        return Err(TaskError::RuntimeFailure(status as u32));
    }
    Ok(removed)
}

/// Bounded task-clockevent result consumed by the runtime clockevent owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TaskClockEventOutcome {
    slice_expired: bool,
    deadline_overrun: bool,
    expired: usize,
    pending: bool,
    update: crate::runtime::TaskDeadlineUpdate,
}

impl TaskClockEventOutcome {
    /// Returns whether the current scheduling slice or budget expired.
    pub const fn slice_expired(self) -> bool {
        self.slice_expired
    }
    /// Returns whether CBS exhaustion entered PI-critical rescue.
    pub const fn deadline_overrun(self) -> bool {
        self.deadline_overrun
    }
    /// Returns the number of timer events stored for safe-point handling.
    pub const fn expired(self) -> usize {
        self.expired
    }
    /// Returns whether another bounded expiry pass is immediately required.
    pub const fn pending(self) -> bool {
        self.pending
    }
    /// Returns the complete generation-ordered task-deadline publication.
    pub const fn update(self) -> crate::runtime::TaskDeadlineUpdate {
        self.update
    }
    /// Returns the next representable task deadline.
    pub const fn next_deadline_ns(self) -> Option<u64> {
        match self.update.deadline() {
            Some(deadline) => Some(deadline.as_nanos()),
            None => None,
        }
    }
}
