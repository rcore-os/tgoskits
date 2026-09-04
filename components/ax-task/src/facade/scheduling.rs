use super::*;
use crate::runtime::RuntimeSwitchPlan;

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
    mut entry: RuntimeSchedulerEntry,
) -> Result<SchedulerOutcome, TaskError> {
    let original_entry = entry;
    loop {
        let request_scope = scheduler_request_scope(entry, original_entry);
        let mut scheduler_frame =
            RuntimeSchedulerFrameGuard::enter(RuntimeScheduleOrigin::Preempt, entry)?;
        let system = scheduler_frame.task_system();
        let current_publication = scheduler_frame.current_thread_publication();
        let (outcome, no_switch_request_pending) = {
            let mut cpu = runtime_current_cpu_mut(&mut scheduler_frame)?;
            // SAFETY: RuntimeSchedulerFrameGuard owns the IRQ-off scheduler baton.
            let current_state = unsafe { cpu.scheduler_current_lifecycle_state() };
            let outcome = if !cpu.scheduler_request_pending(request_scope) && !cpu.has_remote_work()
            {
                if current_state == Some(ThreadState::Parking) {
                    SchedulerOutcome::ParkingDeferred
                } else {
                    SchedulerOutcome::Quiescent
                }
            } else {
                // SAFETY: the publication was captured by this scheduler
                // frame's runtime transaction and remains current here.
                let current = unsafe { current_publication.borrow_current()? };
                // SAFETY: `scheduler_frame` owns the IRQ-off scheduler baton.
                unsafe {
                    system.schedule_if_requested_in_scheduler_frame(
                        cpu.as_mut(),
                        &current,
                        request_scope,
                    )?
                }
            };
            let request_pending = match outcome.decision() {
                Some(decision) if decision.requires_context_switch() => None,
                Some(_) | None => Some(cpu.scheduler_request_pending(request_scope)),
            };
            (outcome, request_pending)
        };
        if let Some(decision) = outcome.decision() {
            execute_switch_plan(&mut scheduler_frame, decision);
        }
        let needs_reschedule = if let Some(request_pending) = no_switch_request_pending {
            request_pending
        } else {
            scheduler_frame.scheduler_request_pending(request_scope)?
        };
        let repeat = preempt_schedule_needs_repeat(&outcome, needs_reschedule);
        drop(scheduler_frame);
        if !repeat {
            return Ok(outcome);
        }
        entry = match entry {
            RuntimeSchedulerEntry::IrqReturn | RuntimeSchedulerEntry::IrqReturnContinuation => {
                RuntimeSchedulerEntry::IrqReturnContinuation
            }
            RuntimeSchedulerEntry::Task
            | RuntimeSchedulerEntry::PreemptExit
            | RuntimeSchedulerEntry::IrqGuardExit => RuntimeSchedulerEntry::Task,
        };
    }
}

fn scheduler_request_scope(
    entry: RuntimeSchedulerEntry,
    original_entry: RuntimeSchedulerEntry,
) -> SchedulerRequestScope {
    if matches!(
        original_entry,
        RuntimeSchedulerEntry::PreemptExit | RuntimeSchedulerEntry::IrqGuardExit
    ) {
        // Linux preempt-enable and task-context IRQ-exit loops continue only
        // while ordinary need-resched remains set. Rewriting the continuation
        // entry to `Task` must not promote the next pass to a lazy-consuming
        // explicit scheduling point.
        return SchedulerRequestScope::Immediate;
    }
    match entry {
        RuntimeSchedulerEntry::Task => SchedulerRequestScope::All,
        RuntimeSchedulerEntry::PreemptExit
        | RuntimeSchedulerEntry::IrqReturn
        | RuntimeSchedulerEntry::IrqGuardExit
        | RuntimeSchedulerEntry::IrqReturnContinuation => SchedulerRequestScope::Immediate,
    }
}

fn preempt_schedule_needs_repeat(outcome: &SchedulerOutcome, needs_reschedule: bool) -> bool {
    needs_reschedule && !outcome.parking_deferred()
}

/// Yields the calling thread and executes the resulting context switch.
pub fn yield_current_cpu() -> Result<(), TaskError> {
    #[cfg(feature = "qperf-metrics")]
    let scheduler_started_ns = task_runtime::monotonic_now().as_nanos();
    let mut scheduler_frame = RuntimeSchedulerFrameGuard::enter(
        RuntimeScheduleOrigin::Yield,
        RuntimeSchedulerEntry::Task,
    )?;
    #[cfg(feature = "qperf-metrics")]
    let scheduler_frame_entered_ns = task_runtime::monotonic_now().as_nanos();
    let system = scheduler_frame.task_system();
    #[cfg(feature = "qperf-metrics")]
    let scheduler_dispatch_started_ns;
    let outcome = {
        let mut cpu = runtime_current_cpu_mut(&mut scheduler_frame)?;
        #[cfg(feature = "qperf-metrics")]
        {
            scheduler_dispatch_started_ns = task_runtime::monotonic_now().as_nanos();
        }
        // SAFETY: `scheduler_frame` owns the IRQ-off scheduler baton.
        unsafe { system.yield_current_in_scheduler_frame(cpu.as_mut())? }
    };
    #[cfg(feature = "qperf-metrics")]
    let scheduler_dispatch_finished_ns = task_runtime::monotonic_now().as_nanos();
    if let Some(decision) = outcome.decision() {
        #[cfg(feature = "qperf-metrics")]
        {
            crate::metrics::qperf_record_switch_scheduler_detail(
                7,
                scheduler_started_ns,
                scheduler_frame_entered_ns,
            );
            crate::metrics::qperf_record_switch_scheduler_detail(
                8,
                scheduler_frame_entered_ns,
                scheduler_dispatch_started_ns,
            );
            crate::metrics::qperf_record_switch_scheduler_detail(
                9,
                scheduler_dispatch_started_ns,
                scheduler_dispatch_finished_ns,
            );
            crate::metrics::qperf_record_switch_phase_scheduler(
                scheduler_started_ns,
                scheduler_dispatch_finished_ns,
            );
        }
        execute_switch_plan(&mut scheduler_frame, decision);
    }
    Ok(())
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
    let current = current_thread_handle()?;
    let mut irq = RuntimeIrqGuard::enter();
    let system = runtime_task_system()?;
    let mut cpu = runtime_current_cpu_mut(&mut irq)?;
    let system = system.prepare_current_exit(cpu.as_mut(), &current)?;
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
    let system = scheduler_frame.task_system();
    let decision = {
        let mut cpu = runtime_current_cpu_mut(&mut scheduler_frame)
            .unwrap_or_else(|_| task_runtime::fatal_invariant(0x4558_0013, thread.as_u64() as _));
        // SAFETY: `scheduler_frame` owns the IRQ-off scheduler baton.
        unsafe { system.commit_prepared_current_exit(cpu.as_mut(), permit.system) }
    };
    execute_switch_plan(&mut scheduler_frame, &decision);
    // An exited context is never re-enqueued, so returning here indicates a
    // broken architecture switch contract.
    task_runtime::fatal_invariant(4, decision.previous().map_or(0, ThreadId::as_u64) as usize)
}

pub(super) fn execute_switch_plan(
    scheduler_frame: &mut RuntimeSchedulerFrameGuard,
    decision: &ScheduleDecision,
) {
    if !decision.requires_context_switch() {
        return;
    }
    #[cfg(feature = "qperf-metrics")]
    let prepare_started_ns = task_runtime::monotonic_now().as_nanos();
    let Some(previous) = decision.previous_endpoint() else {
        task_runtime::fatal_invariant(1, decision.next().as_u64() as usize);
    };
    let next = decision.next_endpoint();
    let plan = RuntimeSwitchPlan::new(
        previous.context(),
        previous.address_space(),
        next.context(),
        next.address_space(),
    )
    .unwrap_or_else(|| task_runtime::fatal_invariant(6, next.thread().as_u64() as usize));
    #[cfg(feature = "qperf-metrics")]
    let switch_validate_finished_ns = task_runtime::monotonic_now().as_nanos();
    // Match Linux's sched_switch observation point: the trace runs while the
    // previous extension is still the published current task, but after all
    // scheduler locks have been released and the switch decision is final.
    task_runtime::trace_sched_switch(SchedSwitchRecord {
        cpu: scheduler_frame.cpu_id(),
        previous_thread: previous.thread().as_u64(),
        next_thread: next.thread().as_u64(),
        timestamp_ns: decision.timestamp_ns(),
        reason: decision.switch_reason() as u32,
    });
    #[cfg(feature = "qperf-metrics")]
    let switch_trace_finished_ns = task_runtime::monotonic_now().as_nanos();
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
    #[cfg(feature = "qperf-metrics")]
    let switch_out_hook_finished_ns = task_runtime::monotonic_now().as_nanos();
    #[cfg(feature = "qperf-metrics")]
    crate::metrics::record_context_switch(decision.switch_reason());
    #[cfg(feature = "qperf-metrics")]
    let switch_accounting_finished_ns = task_runtime::monotonic_now().as_nanos();
    #[cfg(feature = "qperf-metrics")]
    let plan = {
        crate::metrics::qperf_record_switch_scheduler_detail(
            26,
            prepare_started_ns,
            switch_validate_finished_ns,
        );
        crate::metrics::qperf_record_switch_scheduler_detail(
            27,
            switch_validate_finished_ns,
            switch_trace_finished_ns,
        );
        crate::metrics::qperf_record_switch_scheduler_detail(
            28,
            switch_trace_finished_ns,
            switch_out_hook_finished_ns,
        );
        crate::metrics::qperf_record_switch_scheduler_detail(
            29,
            switch_out_hook_finished_ns,
            switch_accounting_finished_ns,
        );
        let mut plan = plan;
        plan.set_qperf_prepare_started_ns(prepare_started_ns);
        plan
    };
    // SAFETY: the scheduler committed both endpoint states before releasing
    // its locks. Every context and address-space handle remains live, and
    // local IRQs stay disabled while the runtime consumes the complete plan.
    unsafe { task_runtime::switch_context(plan) };
    scheduler_frame.refresh_current_cpu();
    // SAFETY: the scheduler frame retains its IRQ-off baton across the
    // architecture switch and through switch-tail completion.
    if unsafe { complete_current_context_switch_tail_in_scheduler_frame(scheduler_frame) }.is_err()
    {
        task_runtime::fatal_invariant(5, 0);
    }
}

/// Completes a fresh context's switch tail below its transferred scheduler
/// baton.
///
/// # Safety
///
/// The caller must be the first instruction sequence of a freshly switched-in
/// context and must retain the transferred IRQ-off scheduler baton until this
/// function returns.
pub(super) unsafe fn complete_current_context_switch_tail(
    pin: &mut impl RuntimeCpuPin,
) -> Result<(), TaskError> {
    let system = runtime_task_system()?;
    let completion = {
        let mut cpu = runtime_current_cpu_mut(pin)?;
        // SAFETY: forwarded from this helper's transferred-baton contract.
        unsafe { system.complete_context_switch_in_scheduler_frame(cpu.as_mut())? }
    };
    completion.finish();
    Ok(())
}

/// Completes the inherited switch tail below a live scheduler frame.
///
/// # Safety
///
/// `scheduler_frame` must retain the IRQ-off scheduler baton until this
/// function returns.
unsafe fn complete_current_context_switch_tail_in_scheduler_frame(
    scheduler_frame: &mut RuntimeSchedulerFrameGuard,
) -> Result<(), TaskError> {
    let system = scheduler_frame.task_system();
    let completion = {
        let mut cpu = runtime_current_cpu_mut(scheduler_frame)?;
        // SAFETY: forwarded from this helper's scheduler-frame contract.
        unsafe { system.complete_context_switch_in_scheduler_frame(cpu.as_mut())? }
    };
    completion.finish();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preempt_exit_continuation_does_not_consume_lazy_requests() {
        assert_eq!(
            scheduler_request_scope(
                RuntimeSchedulerEntry::Task,
                RuntimeSchedulerEntry::PreemptExit,
            ),
            SchedulerRequestScope::Immediate
        );
        assert_eq!(
            scheduler_request_scope(
                RuntimeSchedulerEntry::Task,
                RuntimeSchedulerEntry::IrqGuardExit,
            ),
            SchedulerRequestScope::Immediate
        );
        assert_eq!(
            scheduler_request_scope(RuntimeSchedulerEntry::Task, RuntimeSchedulerEntry::Task),
            SchedulerRequestScope::All
        );
    }
}
