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
    let now_ns = service_scheduler_safe_point_deadlines(system, &mut scheduler_frame)?;
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
            system.schedule_if_requested_after_deadline_service(cpu.as_mut(), now_ns)?
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

pub(super) fn service_scheduler_safe_point_deadlines(
    system: &TaskSystem,
    pin: &mut impl RuntimeCpuPin,
) -> Result<u64, TaskError> {
    let now_ns = task_runtime::monotonic_ns();
    let should_run = {
        let mut cpu = runtime_current_cpu_mut(pin)?;
        let expiry_due = cpu.task_deadline_expiry_due(now_ns);
        if !cpu.deadline_work_pending() && !expiry_due {
            return Ok(now_ns);
        }
        cpu.as_mut().begin_deadline_work() || expiry_due
    };
    if !should_run {
        return Ok(now_ns);
    }

    let result = (|| {
        // Empty the bounded IRQ buffer before promoting another batch. This
        // guarantees progress even when the clockevent filled every slot,
        // while still entering the heap expiry engine at most once per safe
        // point.
        let mut drained = drain_current_expired_timers(system, pin)?;
        let batch_pending = {
            let mut cpu = runtime_current_cpu_mut(pin)?;
            if cpu.task_deadline_expiry_due(now_ns) {
                let budget = cpu.batch_limit();
                cpu.as_mut()
                    .expire_task_deadlines(now_ns, task_runtime::timer_resolution_ns(), budget)
                    .pending()
            } else {
                false
            }
        };
        drained += drain_current_expired_timers(system, pin)?;
        let mut cpu = runtime_current_cpu_mut(pin)?;
        let pending = batch_pending
            || cpu.has_expired_task_deadlines()
            || cpu.task_deadline_expiry_due(now_ns);
        cpu.as_mut().finish_deadline_work(pending);
        Ok(drained)
    })();
    if result.is_err() {
        let mut cpu = runtime_current_cpu_mut(pin)?;
        cpu.as_mut().finish_deadline_work(true);
    }
    let drained = result?;
    let scheduler_events = {
        let mut cpu = runtime_current_cpu_mut(pin)?;
        system.service_pending_deadline_timers(cpu.as_mut(), now_ns)?
    };
    if drained + scheduler_events == 0 {
        Ok(now_ns)
    } else {
        Ok(task_runtime::monotonic_ns().max(now_ns))
    }
}

/// Yields the calling thread and executes the resulting context switch.
pub fn yield_current_cpu() -> Result<ScheduleDecision, TaskError> {
    validate_schedule_context(RuntimeScheduleOrigin::Yield)?;
    let mut scheduler_frame = RuntimeSchedulerFrameGuard::enter(
        RuntimeScheduleOrigin::Yield,
        RuntimeSchedulerEntry::Task,
    )?;
    let system = runtime_task_system()?;
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
    system: CurrentExitPermit,
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
    let system = system.prepare_current_exit(cpu.as_mut(), now_ns)?;
    Ok(ExitPermit {
        system,
        _not_send: PhantomData,
    })
}

/// Commits a prepared scheduler exit and permanently leaves this context.
///
/// Any failure after completion became externally visible is a fatal runtime
/// invariant; this function therefore has no recoverable return path.
pub fn commit_current_exit(permit: ExitPermit) -> ! {
    let thread = permit.system.thread();
    let mut scheduler_frame =
        RuntimeSchedulerFrameGuard::enter(RuntimeScheduleOrigin::Exit, RuntimeSchedulerEntry::Task)
            .unwrap_or_else(|_| task_runtime::fatal_invariant(0x4558_0010, thread.as_u64() as _));
    let system = runtime_task_system()
        .unwrap_or_else(|_| task_runtime::fatal_invariant(0x4558_0011, thread.as_u64() as _));
    let now_ns = task_runtime::monotonic_ns();
    let decision = {
        let mut cpu = runtime_current_cpu_mut(&mut scheduler_frame)
            .unwrap_or_else(|_| task_runtime::fatal_invariant(0x4558_0013, thread.as_u64() as _));
        if cpu.current() != Some(thread) {
            task_runtime::fatal_invariant(0x4558_0014, thread.as_u64() as _);
        }
        system
            .commit_prepared_current_exit(cpu.as_mut(), permit.system, now_ns)
            .unwrap_or_else(|_| task_runtime::fatal_invariant(0x4558_0015, thread.as_u64() as _))
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
    prepare_next_address_space(next.address_space(), next.thread());
    #[cfg(feature = "qperf-metrics")]
    crate::metrics::record_context_switch();
    // SAFETY: the scheduler committed both endpoint states before releasing its
    // locks. Runtime handles remain live, and local IRQs stay disabled here.
    unsafe { task_runtime::switch_context(previous.context(), next.context()) };
    scheduler_frame.refresh_current_cpu();
    if complete_current_context_switch_tail(scheduler_frame).is_err() {
        task_runtime::fatal_invariant(5, 0);
    }
}

fn activate_next_address_space(
    address_space: crate::runtime::AddressSpaceHandle,
    thread: ThreadId,
) {
    let activation = crate::runtime::AddressSpaceActivation::for_thread(address_space);
    if task_runtime::activate_address_space(activation) != crate::runtime::RuntimeStatus::Success {
        task_runtime::fatal_invariant(3, thread.as_u64() as usize);
    }
}

pub(super) fn prepare_next_address_space(
    address_space: crate::runtime::AddressSpaceHandle,
    thread: ThreadId,
) {
    activate_next_address_space(address_space, thread);
}

pub(super) fn complete_current_context_switch_tail(
    pin: &mut impl RuntimeCpuPin,
) -> Result<(), TaskError> {
    let system = runtime_task_system()?;
    let completion = {
        let mut cpu = runtime_current_cpu_mut(pin)?;
        system.complete_context_switch(cpu.as_mut())?
    };
    completion.finish();
    Ok(())
}
