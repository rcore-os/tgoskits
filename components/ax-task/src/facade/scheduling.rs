use super::*;

/// Runs one scheduler decision at a task/IRQ-return safe point.
///
/// The typed outcome distinguishes a completed decision, an in-flight park
/// handshake, and bounded owner-work backpressure. It never clears
/// `need_resched` before entering the scheduler.
///
/// # Errors
///
/// Returns [`TaskError::UnsafeContext`] in hard IRQ context and object-handle
/// errors when runtime initialization is incomplete or inconsistent.
pub fn schedule_current_cpu() -> Result<SchedulerOutcome, TaskError> {
    validate_schedule_context(RuntimeScheduleOrigin::Preempt)?;
    schedule_current_cpu_with_entry(RuntimeSchedulerEntry::Task)
}

/// Services the final preemption-guard exit without exposing a preemptible
/// window before the scheduler owns its CPU-local baton.
///
/// # Safety
///
/// `entry` must match the caller's exact runtime context. The caller must own
/// one final lock-preemption depth and must satisfy the raw IRQ-state contract
/// documented by [`RuntimeSchedulerEntry`].
pub unsafe fn schedule_current_cpu_from_preempt_exit(
    entry: RuntimeSchedulerEntry,
) -> Result<SchedulerOutcome, TaskError> {
    if !matches!(
        entry,
        RuntimeSchedulerEntry::PreemptExit | RuntimeSchedulerEntry::IrqReturn
    ) {
        return Err(TaskError::UnsafeContext);
    }
    schedule_current_cpu_with_entry(entry)
}

/// Services the final task-context IRQ publication guard exit without
/// restoring IRQs before the scheduler owns its CPU-local baton.
///
/// # Safety
///
/// The caller must own the final runtime IRQ-guard depth, have entered it from
/// ordinary task context with IRQs enabled, and retain raw IRQ exclusion.
pub unsafe fn schedule_current_cpu_from_irq_guard_exit() -> Result<SchedulerOutcome, TaskError> {
    schedule_current_cpu_with_entry(RuntimeSchedulerEntry::IrqGuardExit)
}

fn schedule_current_cpu_with_entry(
    entry: RuntimeSchedulerEntry,
) -> Result<SchedulerOutcome, TaskError> {
    let mut scheduler_frame =
        RuntimeSchedulerFrameGuard::enter(RuntimeScheduleOrigin::Preempt, entry)?;
    let system = runtime_task_system()?;
    let deadline_now_ns = task_runtime::monotonic_ns();
    service_current_task_deadline_work(system, &mut scheduler_frame, deadline_now_ns)?;
    let now_ns = task_runtime::monotonic_ns();
    let outcome = {
        let mut cpu = runtime_current_cpu_mut(&mut scheduler_frame)?;
        let current_state = cpu.current_lifecycle_state();
        if !cpu.needs_reschedule() && !cpu.has_remote_work() {
            if current_state == Some(ThreadState::Parking) {
                SchedulerOutcome::ParkingDeferred
            } else {
                SchedulerOutcome::Quiescent
            }
        } else {
            system.schedule_if_requested(cpu.as_mut(), now_ns)?
        }
    };
    if let Some(decision) = outcome.decision() {
        execute_switch_plan(&mut scheduler_frame, decision, now_ns);
    }
    Ok(outcome)
}

pub(super) fn drain_current_expired_timers(
    system: &TaskSystem,
    pin: &mut impl RuntimeCpuPin,
) -> Result<usize, TaskError> {
    let mut drained = 0;
    loop {
        let event = {
            let mut cpu = runtime_current_cpu_mut(pin)?;
            cpu.as_mut().take_expired_park_deadline()
        };
        let Some(event) = event else {
            break;
        };
        let Some(thread) = event.thread() else {
            continue;
        };
        match system.thread_handle(thread) {
            Ok(handle) => {
                let completed = handle.core.complete_sleep_timer(event.token().generation());
                let park_matches = event.kind().is_some_and(|kind| {
                    kind.park_generation() == Some(handle.core.park_generation())
                });
                if completed && park_matches {
                    let _wake_result = handle.wake_handle().wake();
                }
            }
            Err(TaskError::StaleThreadId) => {}
            Err(error) => return Err(error),
        }
        drained += 1;
    }
    Ok(drained)
}

pub(super) fn service_current_task_deadline_work(
    system: &TaskSystem,
    pin: &mut impl RuntimeCpuPin,
    now_ns: u64,
) -> Result<usize, TaskError> {
    let should_run = {
        let mut cpu = runtime_current_cpu_mut(pin)?;
        // The physical clockevent is only an acceleration mechanism. A lost,
        // late, or stopped device edge must not strand an already-due task
        // deadline while another scheduler condition keeps the CPU out of its
        // final idle wait. Promote one bounded batch before claiming the
        // sticky owner-work doorbell so every scheduler entry is also a
        // deadline recovery safe point.
        let budget = cpu.batch_limit();
        cpu.as_mut()
            .expire_task_deadlines(now_ns, task_runtime::timer_resolution_ns(), budget);
        cpu.as_mut().begin_deadline_work()
    };
    if !should_run {
        return Ok(0);
    }

    let result = (|| {
        let mut drained = drain_current_expired_timers(system, pin)?;
        let batch = {
            let mut cpu = runtime_current_cpu_mut(pin)?;
            let budget = cpu.batch_limit();
            cpu.as_mut()
                .expire_task_deadlines(now_ns, task_runtime::timer_resolution_ns(), budget)
        };
        drained += drain_current_expired_timers(system, pin)?;
        let mut cpu = runtime_current_cpu_mut(pin)?;
        let pending = batch.pending() || cpu.has_expired_task_deadlines();
        cpu.as_mut().finish_deadline_work(pending);
        Ok(drained)
    })();
    if result.is_err() {
        let mut cpu = runtime_current_cpu_mut(pin)?;
        cpu.as_mut().finish_deadline_work(true);
    }
    result
}

/// Yields the calling thread and executes the resulting context switch.
pub fn yield_current_cpu() -> Result<ScheduleDecision, TaskError> {
    validate_schedule_context(RuntimeScheduleOrigin::Yield)?;
    let mut scheduler_frame = RuntimeSchedulerFrameGuard::enter(
        RuntimeScheduleOrigin::Yield,
        RuntimeSchedulerEntry::Task,
    )?;
    let system = runtime_task_system()?;
    let deadline_now_ns = task_runtime::monotonic_ns();
    service_current_task_deadline_work(system, &mut scheduler_frame, deadline_now_ns)?;
    let now_ns = task_runtime::monotonic_ns();
    let decision = {
        let mut cpu = runtime_current_cpu_mut(&mut scheduler_frame)?;
        system.yield_current(cpu.as_mut(), now_ns)?
    };
    execute_switch_plan(&mut scheduler_frame, decision, now_ns);
    Ok(decision)
}

/// Exits the calling thread and switches to its replacement.
pub fn exit_current_thread() -> Result<(), TaskError> {
    let permit = prepare_current_exit()?;
    commit_current_exit(permit)
}

/// A validated, thread-bound opportunity to publish exit completion.
pub struct ExitPermit {
    thread: ThreadId,
    _not_send: PhantomData<*mut ()>,
}

/// Validates scheduler-side exit prerequisites without changing the current
/// thread's observable lifecycle.
pub fn prepare_current_exit() -> Result<ExitPermit, TaskError> {
    validate_schedule_context(RuntimeScheduleOrigin::Exit)?;
    let mut irq = RuntimeIrqGuard::enter();
    let system = runtime_task_system()?;
    let now_ns = task_runtime::monotonic_ns();
    let mut cpu = runtime_current_cpu_mut(&mut irq)?;
    let thread = system.prepare_current_exit(cpu.as_mut(), now_ns)?;
    Ok(ExitPermit {
        thread,
        _not_send: PhantomData,
    })
}

/// Commits a prepared scheduler exit and permanently leaves this context.
///
/// Any failure after completion became externally visible is a fatal runtime
/// invariant; this function therefore has no recoverable return path.
pub fn commit_current_exit(permit: ExitPermit) -> ! {
    let mut scheduler_frame =
        RuntimeSchedulerFrameGuard::enter(RuntimeScheduleOrigin::Exit, RuntimeSchedulerEntry::Task)
            .unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x4558_0010, permit.thread.as_u64() as _)
            });
    let system = runtime_task_system().unwrap_or_else(|_| {
        task_runtime::fatal_invariant(0x4558_0011, permit.thread.as_u64() as _)
    });
    let deadline_now_ns = task_runtime::monotonic_ns();
    service_current_task_deadline_work(system, &mut scheduler_frame, deadline_now_ns)
        .unwrap_or_else(|_| {
            task_runtime::fatal_invariant(0x4558_0012, permit.thread.as_u64() as _)
        });
    let now_ns = task_runtime::monotonic_ns();
    let decision = {
        let mut cpu = runtime_current_cpu_mut(&mut scheduler_frame).unwrap_or_else(|_| {
            task_runtime::fatal_invariant(0x4558_0013, permit.thread.as_u64() as _)
        });
        if cpu.current() != Some(permit.thread) {
            task_runtime::fatal_invariant(0x4558_0014, permit.thread.as_u64() as _);
        }
        system.exit_current(cpu.as_mut()).unwrap_or_else(|_| {
            task_runtime::fatal_invariant(0x4558_0015, permit.thread.as_u64() as _)
        })
    };
    execute_switch_plan(&mut scheduler_frame, decision, now_ns);
    // An exited context is never re-enqueued, so returning here indicates a
    // broken architecture switch contract.
    task_runtime::fatal_invariant(4, decision.previous().map_or(0, ThreadId::as_u64) as usize)
}

pub(super) fn execute_switch_plan(
    scheduler_frame: &mut RuntimeSchedulerFrameGuard,
    decision: ScheduleDecision,
    now_ns: u64,
) {
    if !decision.requires_context_switch() {
        return;
    }
    let Some(previous) = decision.previous_endpoint() else {
        task_runtime::fatal_invariant(1, decision.next().as_u64() as usize);
    };
    let next = decision.next_endpoint();
    if previous.context().is_none() || next.context().is_none() {
        task_runtime::fatal_invariant(2, next.thread().as_u64() as usize);
    }
    // Match Linux's sched_switch observation point: the trace runs while the
    // previous extension is still the published current task, but after all
    // scheduler locks have been released and the switch decision is final.
    task_runtime::trace_sched_switch(SchedSwitchRecord {
        // SAFETY: scheduler_frame retains the current CPU's scheduler baton.
        cpu: RuntimeCpuId::new(unsafe { task_runtime::current_cpu_id() }.as_u32()),
        previous_thread: previous.thread().as_u64(),
        next_thread: next.thread().as_u64(),
        timestamp_ns: now_ns,
        reason: decision.switch_reason() as u32,
    });
    if let Some(extension) = previous.extension() {
        // SAFETY: ThreadExtension construction guarantees callback validity;
        // TaskSystem released every internal lock and the scheduler baton
        // keeps local IRQs disabled.
        unsafe {
            (extension.ops().on_switch_out)(
                extension.data(),
                previous.thread(),
                decision.switch_reason(),
            )
        };
    }
    prepare_next_context(next.address_space(), next.thread(), next.extension());
    // SAFETY: the scheduler committed both endpoint states before releasing its
    // locks. Runtime handles remain live, and local IRQs stay disabled here.
    unsafe { task_runtime::switch_context(previous.context(), next.context()) };
    scheduler_frame.refresh_current_cpu();
    if complete_current_context_switch_tail(scheduler_frame).is_err() {
        task_runtime::fatal_invariant(5, 0);
    }
}

fn install_next_address_space(address_space: crate::runtime::AddressSpaceHandle, thread: ThreadId) {
    if task_runtime::install_address_space(address_space) != crate::runtime::RuntimeStatus::Success
    {
        task_runtime::fatal_invariant(3, thread.as_u64() as usize);
    }
}

pub(super) fn prepare_next_context(
    address_space: crate::runtime::AddressSpaceHandle,
    thread: ThreadId,
    extension: Option<crate::ThreadExtensionView>,
) {
    install_next_address_space(address_space, thread);
    if let Some(extension) = extension {
        // SAFETY: ThreadExtension construction guarantees callback validity;
        // the address space is now active and no scheduler lock is held.
        unsafe { (extension.ops().on_switch_in)(extension.data(), thread) };
    }
}

pub(super) fn complete_current_context_switch_tail(
    pin: &mut impl RuntimeCpuPin,
) -> Result<(), TaskError> {
    let system = runtime_task_system()?;
    let mut cpu = runtime_current_cpu_mut(pin)?;
    system.complete_context_switch(cpu.as_mut())
}
