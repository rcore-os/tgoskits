//! Per-CPU task and kernel timer service.

use alloc::{boxed::Box, format, vec::Vec};
use core::{
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    task::{Context, Poll},
};

use ax_hal::time::{TimeValue, monotonic_time};
use ax_timer_list::{TimerEvent, TimerList};

#[cfg(feature = "smp")]
use crate::select_run_queue;
use crate::{
    AxCpuMask, AxTaskRef, IrqNotify, TaskInner, current_run_queue,
    future::time::{FutureTimerHandle, TimerRuntime},
    sync::{PreemptIrqSaveGuard, RawState, SpinLock},
};

mod clock_event;
mod kernel;

pub use clock_event::ClockEventControl;
pub use kernel::{
    HardKernelTimerAction, HardKernelTimerCallback, KernelTimerAction, KernelTimerCallback,
    KernelTimerCancelOutcome, KernelTimerError, KernelTimerHandle, MonotonicDeadline,
    MonotonicInstant, RestartableKernelTimerCallback, TimerCpuId,
};
use kernel::{KernelTimerEntry, KernelTimerQueue, KernelTimerQueueCancel};

const KERNEL_TIMER_CAPACITY: usize = 1024;
const TIMER_IRQ_BUDGET: usize = 64;

static TIMER_TICKET_ID: AtomicU64 = AtomicU64::new(1);

percpu_static! {
    TIMER_BASE: SpinLock<PerCpuTimerBase> = SpinLock::new(PerCpuTimerBase::new()),
    TIMER_NOTIFY: IrqNotify = IrqNotify::new(),
    TIMER_WORKER_STARTED: AtomicBool = AtomicBool::new(false),
    TIMER_CALLBACKS: Vec<Box<dyn Fn(TimeValue) + Send + Sync>> = Vec::new(),
}

/// One CPU's logical monotonic timer owner.
///
/// The typed queues keep their payload semantics separate while sharing one
/// owner lock and one earliest-deadline publication path. The physical
/// comparator is deliberately not part of this object; `ax-runtime` owns it.
struct PerCpuTimerBase {
    task_wakeups: TimerList<TaskWakeupEvent>,
    future_wakeups: TimerRuntime,
    kernel_timers: KernelTimerQueue,
}

impl PerCpuTimerBase {
    const fn new() -> Self {
        Self {
            task_wakeups: TimerList::new(),
            future_wakeups: TimerRuntime::new(),
            kernel_timers: KernelTimerQueue::new(KERNEL_TIMER_CAPACITY),
        }
    }

    fn next_deadline(&self) -> Option<TimeValue> {
        [
            self.task_wakeups.next_deadline(),
            self.future_wakeups.next_deadline(),
            self.kernel_timers
                .next_soft_deadline()
                .map(MonotonicDeadline::as_duration),
            self.kernel_timers
                .next_hard_deadline()
                .map(MonotonicDeadline::as_duration),
        ]
        .into_iter()
        .flatten()
        .min()
    }
}

struct TaskWakeupEvent {
    ticket_id: u64,
    task: AxTaskRef,
}

impl TimerEvent for TaskWakeupEvent {
    fn callback(self, _now: TimeValue) {
        // Ignore the timer event if timeout was set but not triggered
        // (wake up by `WaitQueue::notify()`).
        // Judge if this timer event is still valid by checking the ticket ID.
        if self.task.timer_ticket() != self.ticket_id {
            // Timer ticket ID is not matched.
            // Just ignore this timer event and return.
            return;
        }

        // Timer ticket match. Timers are per-CPU, so prefer waking the task on
        // the CPU that owns and expires this timer event. Falling back to the
        // affinity selector is only needed if the task's affinity changed while
        // it was sleeping.
        wake_task_from_timer(self.task)
    }
}

#[cfg(feature = "smp")]
fn wake_task_from_timer(task: AxTaskRef) {
    if task.cpumask().get(ax_hal::percpu::this_cpu_id()) {
        current_run_queue::<RawState>().unblock_task(task, true);
    } else {
        select_run_queue::<RawState>(&task).unblock_task(task, true);
    }
}

#[cfg(not(feature = "smp"))]
fn wake_task_from_timer(task: AxTaskRef) {
    current_run_queue::<RawState>().unblock_task(task, true);
}

/// Registers a callback function to be called on each timer tick.
pub fn register_timer_callback<F>(callback: F)
where
    F: Fn(TimeValue) + Send + Sync + 'static,
{
    with_local_exclusive(|exclusive| {
        TIMER_CALLBACKS.with_current_mut(exclusive, |callbacks| callbacks.push(Box::new(callback)))
    });
}

fn check_callbacks() {
    with_local_pin(|pin| {
        TIMER_CALLBACKS.with_current(pin, |callbacks| {
            for callback in callbacks {
                callback(monotonic_time());
            }
        })
    });
}

fn deadline_to_nanos(deadline: TimeValue) -> u64 {
    deadline.as_nanos().min(u64::MAX as u128) as u64
}

pub(crate) fn maybe_reprogram_timer(deadline: TimeValue) {
    clock_event::publish_earlier_deadline(deadline_to_nanos(deadline));
}

pub(crate) fn next_deadline_nanos() -> Option<u64> {
    with_current_timer_base(|timer_base| timer_base.next_deadline().map(deadline_to_nanos))
}

pub(crate) fn set_alarm_wakeup(deadline: TimeValue, task: AxTaskRef) {
    let _owner_guard = PreemptIrqSaveGuard::new();
    with_current_timer_base(|timer_base| {
        let ticket_id = TIMER_TICKET_ID.fetch_add(1, Ordering::AcqRel);
        task.set_timer_ticket(ticket_id);
        timer_base
            .task_wakeups
            .set(deadline, TaskWakeupEvent { ticket_id, task });
    });
    maybe_reprogram_timer(deadline);
}

pub(crate) fn register_future_timer(deadline: TimeValue) -> Option<FutureTimerHandle> {
    let _owner_guard = PreemptIrqSaveGuard::new();
    let owner_cpu = ax_hal::percpu::this_cpu_id();
    let key = with_timer_base(owner_cpu, |timer_base| {
        timer_base.future_wakeups.add(deadline)
    })?;
    maybe_reprogram_timer(deadline);
    Some(FutureTimerHandle::new(owner_cpu, key))
}

pub(crate) fn poll_future_timer(handle: FutureTimerHandle, context: &mut Context<'_>) -> Poll<()> {
    with_timer_base(handle.owner_cpu(), |timer_base| {
        timer_base.future_wakeups.poll(&handle.key(), context)
    })
}

pub(crate) fn cancel_future_timer(handle: FutureTimerHandle) {
    with_timer_base(handle.owner_cpu(), |timer_base| {
        timer_base.future_wakeups.cancel(&handle.key())
    });
}

/// Registers a one-shot callback on the calling CPU's shared timer base.
///
/// The callback runs in `ktimers/<cpu>` task context without the timer-base
/// lock held.
pub fn register_kernel_timer(
    deadline: MonotonicDeadline,
    callback: KernelTimerCallback,
) -> Result<KernelTimerHandle, KernelTimerError> {
    validate_kernel_timer_context()?;
    register_kernel_timer_entry(KernelTimerEntry::new(deadline, callback)?)
}

/// Registers a task-context callback that can rearm the same timer identity.
pub fn register_restartable_kernel_timer(
    deadline: MonotonicDeadline,
    callback: RestartableKernelTimerCallback,
) -> Result<KernelTimerHandle, KernelTimerError> {
    validate_kernel_timer_context()?;
    register_kernel_timer_entry(KernelTimerEntry::new_restartable(deadline, callback)?)
}

/// Registers a stable callback with explicit hard-IRQ expiry semantics.
pub fn register_hard_restartable_kernel_timer(
    deadline: MonotonicDeadline,
    callback: HardKernelTimerCallback,
) -> Result<KernelTimerHandle, KernelTimerError> {
    validate_kernel_timer_context()?;
    register_kernel_timer_entry(KernelTimerEntry::new_hard_restartable(deadline, callback)?)
}

/// Rearms an inactive hard timer on its registration CPU.
pub fn arm_hard_kernel_timer(
    handle: KernelTimerHandle,
    deadline: MonotonicDeadline,
) -> Result<(), KernelTimerError> {
    validate_kernel_timer_context()?;
    let _owner_guard = PreemptIrqSaveGuard::new();
    let current_cpu = ax_hal::percpu::this_cpu_id();
    if handle.owner().as_usize() != current_cpu {
        return Err(KernelTimerError::OwnerMismatch {
            expected: handle.owner().as_usize(),
            actual: current_cpu,
        });
    }
    let armed = try_with_timer_base(current_cpu, |timer_base| {
        timer_base.kernel_timers.arm_hard(handle, deadline)
    })?;
    if !armed {
        return Err(KernelTimerError::StaleHandle);
    }
    maybe_reprogram_timer(deadline.as_duration());
    Ok(())
}

/// Disarms a stable hard timer without destroying its callback payload.
pub fn disarm_hard_kernel_timer(handle: KernelTimerHandle) -> Result<(), KernelTimerError> {
    validate_kernel_timer_context()?;
    let owner_cpu = handle.owner().as_usize();
    let found = try_with_timer_base(owner_cpu, |timer_base| {
        timer_base.kernel_timers.disarm_hard(handle).is_some()
    })?;
    found.then_some(()).ok_or(KernelTimerError::StaleHandle)
}

/// Cancels a kernel-timer registration without waiting for an executing callback.
///
/// A callback already claimed for execution may finish, but its tombstone
/// prevents any restartable action from returning to the active queue.
pub fn cancel_kernel_timer(
    handle: KernelTimerHandle,
) -> Result<KernelTimerCancelOutcome, KernelTimerError> {
    validate_kernel_timer_context()?;
    let owner_cpu = handle.owner().as_usize();
    let outcome = try_with_timer_base(owner_cpu, |timer_base| {
        timer_base.kernel_timers.cancel(handle)
    })?;
    match outcome {
        KernelTimerQueueCancel::Cancelled(entry) => {
            drop(entry);
            Ok(KernelTimerCancelOutcome::Cancelled)
        }
        KernelTimerQueueCancel::Executing => Ok(KernelTimerCancelOutcome::NotCancelled),
        KernelTimerQueueCancel::Stale => Err(KernelTimerError::StaleHandle),
    }
}

fn register_kernel_timer_entry(
    entry: KernelTimerEntry,
) -> Result<KernelTimerHandle, KernelTimerError> {
    let _owner_guard = PreemptIrqSaveGuard::new();
    let owner_cpu = ax_hal::percpu::this_cpu_id();
    let deadline = entry
        .deadline_for_registration()
        .expect("new kernel timer must start armed");
    let result = try_with_timer_base(owner_cpu, |timer_base| {
        timer_base
            .kernel_timers
            .insert(TimerCpuId::new(owner_cpu), entry)
    })?;
    let handle = result.map_err(|_entry| KernelTimerError::Capacity { cpu_id: owner_cpu })?;
    maybe_reprogram_timer(deadline.as_duration());
    Ok(handle)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum KernelTimerCallContext {
    HardIrq,
    Thread,
    ThreadCriticalSection,
}

fn current_kernel_timer_call_context() -> KernelTimerCallContext {
    if ax_hal::irq::in_irq_context() {
        KernelTimerCallContext::HardIrq
    } else if crate::in_atomic_context() {
        KernelTimerCallContext::ThreadCriticalSection
    } else {
        KernelTimerCallContext::Thread
    }
}

fn validate_kernel_timer_call_context(
    context: KernelTimerCallContext,
) -> Result<(), KernelTimerError> {
    if context == KernelTimerCallContext::HardIrq {
        Err(KernelTimerError::UnsafeContext)
    } else {
        Ok(())
    }
}

fn validate_kernel_timer_context() -> Result<(), KernelTimerError> {
    validate_kernel_timer_call_context(current_kernel_timer_call_context())
}

// SAFETY: only called in timer irq handler, so irq and preemption are
// both disabled here.
pub fn check_events(run_callbacks: bool) {
    if run_callbacks {
        check_callbacks();
    }
    let mut remaining_budget = TIMER_IRQ_BUDGET;
    let hard_now = MonotonicInstant::from_duration(monotonic_time())
        .expect("host monotonic clock must remain finite");
    let mut hard_completed = false;
    let mut hard_expired = 0;
    while remaining_budget != 0 {
        let execution =
            with_current_timer_base(|timer_base| timer_base.kernel_timers.claim_due_hard(hard_now));
        let Some(mut execution) = execution else {
            break;
        };
        let action = unsafe {
            // SAFETY: the timer IRQ owns the current CPU's hard-expiry pass,
            // local IRQs are disabled, and the base lock was released before
            // invoking the explicitly audited callback capability.
            execution.invoke_hard()
        };
        hard_completed |= with_current_timer_base(|timer_base| {
            timer_base
                .kernel_timers
                .complete_hard_execution(execution, action)
        });
        hard_expired += 1;
        remaining_budget -= 1;
    }

    let mut task_expired = 0;
    while remaining_budget != 0 {
        let now = monotonic_time();
        let event = with_current_timer_base(|timer_base| timer_base.task_wakeups.expire_one(now));
        if let Some((_deadline, event)) = event {
            event.callback(now);
            task_expired += 1;
            remaining_budget -= 1;
        } else {
            break;
        }
    }

    let soft_now = MonotonicInstant::from_duration(monotonic_time())
        .expect("host monotonic clock must remain finite");
    let (soft_due, future_due, soft_promoted, soft_pending) =
        with_current_timer_base(|timer_base| {
            let future_due = timer_base
                .future_wakeups
                .publish_due_work(soft_now.as_duration());
            let kernel = timer_base
                .kernel_timers
                .expire_due_soft(soft_now, remaining_budget);
            let soft_due = future_due
                || kernel.expired() != 0
                || kernel.pending()
                || timer_base.kernel_timers.has_expired()
                || timer_base.kernel_timers.has_completed();
            (soft_due, future_due, kernel.expired(), kernel.pending())
        });
    trace!(
        "timer IRQ CPU {}: hard_expired={}, task_expired={}, soft_promoted={}, future_due={}, \
         soft_pending={}, budget_left={}",
        ax_hal::percpu::this_cpu_id(),
        hard_expired,
        task_expired,
        soft_promoted,
        future_due,
        soft_pending,
        remaining_budget
    );
    if soft_due || hard_completed {
        current_timer_notify().notify_irq();
    }
}

/// Starts the owner CPU's shared soft-timer service.
///
/// This must run after the scheduler and per-CPU area are initialized, but
/// before the local timer IRQ is enabled.
pub fn init_timer_service() {
    let cpu_id = ax_hal::percpu::this_cpu_id();
    if timer_worker_started(cpu_id)
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }

    with_timer_base(cpu_id, |timer_base| {
        timer_base
            .kernel_timers
            .reserve_transition_capacity(cpu_id)
            .unwrap_or_else(|error| {
                panic!("failed to reserve CPU {cpu_id} kernel timer queues: {error}")
            });
    });

    let notify = timer_notify(cpu_id);
    let worker = TaskInner::new(
        move || loop {
            notify.wait();
            drain_soft_timer_events(cpu_id);
        },
        format!("ktimers/{cpu_id}"),
        crate::default_task_stack_size(),
    );
    let mut affinity = AxCpuMask::new();
    affinity.set(cpu_id, true);
    worker.set_cpumask(affinity);
    crate::spawn_task(worker);
}

fn drain_soft_timer_events(cpu_id: usize) {
    let mut processed = 0;
    while processed < TIMER_IRQ_BUDGET {
        if let Some(waker) = with_timer_base(cpu_id, |timer_base| {
            timer_base.future_wakeups.expire_one(monotonic_time())
        }) {
            waker.wake();
            processed += 1;
            continue;
        }

        if let Some(mut execution) = with_timer_base(cpu_id, |timer_base| {
            timer_base.kernel_timers.claim_expired()
        }) {
            let action = execution.invoke_soft();
            let completed = with_timer_base(cpu_id, |timer_base| {
                timer_base
                    .kernel_timers
                    .complete_soft_execution(execution, action)
            });
            drop(completed);
            processed += 1;
            continue;
        }

        if let Some(completed) = with_timer_base(cpu_id, |timer_base| {
            timer_base.kernel_timers.claim_completed()
        }) {
            drop(completed);
            processed += 1;
            continue;
        }
        break;
    }

    let now = monotonic_time();
    let pending = with_timer_base(cpu_id, |timer_base| {
        timer_base.future_wakeups.finish_due_work(now)
            || timer_base.kernel_timers.has_expired()
            || timer_base.kernel_timers.has_completed()
    });
    if pending {
        timer_notify(cpu_id).notify();
        crate::yield_now();
    } else if cpu_id == ax_hal::percpu::this_cpu_id()
        && let Some(deadline) = with_timer_base(cpu_id, |timer_base| timer_base.next_deadline())
    {
        maybe_reprogram_timer(deadline);
    }
}

fn with_current_timer_base<R>(operation: impl FnOnce(&mut PerCpuTimerBase) -> R) -> R {
    with_timer_base(ax_hal::percpu::this_cpu_id(), operation)
}

fn with_timer_base<R>(cpu_id: usize, operation: impl FnOnce(&mut PerCpuTimerBase) -> R) -> R {
    operation(&mut timer_base(cpu_id).lock_irqsave())
}

fn try_with_timer_base<R>(
    cpu_id: usize,
    operation: impl FnOnce(&mut PerCpuTimerBase) -> R,
) -> Result<R, KernelTimerError> {
    let timer_base = try_timer_base(cpu_id)?;
    Ok(operation(&mut timer_base.lock_irqsave()))
}

fn timer_base(cpu_id: usize) -> &'static SpinLock<PerCpuTimerBase> {
    try_timer_base(cpu_id).unwrap_or_else(|error| panic!("timer-base access failed: {error}"))
}

fn try_timer_base(cpu_id: usize) -> Result<&'static SpinLock<PerCpuTimerBase>, KernelTimerError> {
    let area = try_timer_cpu_area(cpu_id)?;
    // SAFETY: every installed CPU area contains a process-lifetime TIMER_BASE
    // object, including the scheduler-start-to-timer-service window. Allowing
    // enqueue during that window is required because the GC task is created by
    // `init_scheduler`; `init_timer_service` starts the worker before timer IRQs
    // are enabled and the first clockevent publication includes queued work.
    // All mutable access is serialized by the base's IRQ-save lock.
    Ok(unsafe { TIMER_BASE.remote_ptr(area).as_ref() })
}

fn timer_notify(cpu_id: usize) -> &'static IrqNotify {
    let area = timer_cpu_area(cpu_id);
    // SAFETY: the per-CPU area remains installed for the kernel lifetime and
    // IrqNotify provides its own synchronization for cross-context access.
    unsafe { TIMER_NOTIFY.remote_ptr(area).as_ref() }
}

fn current_timer_notify() -> &'static IrqNotify {
    timer_notify(ax_hal::percpu::this_cpu_id())
}

fn timer_worker_started(cpu_id: usize) -> &'static AtomicBool {
    let area = timer_cpu_area(cpu_id);
    // SAFETY: AtomicBool supports concurrent shared access and the per-CPU
    // storage remains live for the kernel lifetime.
    unsafe { TIMER_WORKER_STARTED.remote_ptr(area).as_ref() }
}

fn timer_cpu_area(cpu_id: usize) -> ax_percpu::PerCpuArea {
    try_timer_cpu_area(cpu_id)
        .unwrap_or_else(|error| panic!("timer CPU area access failed: {error}"))
}

fn try_timer_cpu_area(cpu_id: usize) -> Result<ax_percpu::PerCpuArea, KernelTimerError> {
    let cpu_index = ax_percpu::CpuIndex::try_from(cpu_id)
        .map_err(|_| KernelTimerError::CpuUnavailable { cpu_id })?;
    ax_percpu::area(cpu_index).map_err(|_| KernelTimerError::CpuUnavailable { cpu_id })
}

fn with_local_pin<R>(
    operation: impl for<'scope> FnOnce(&ax_hal::percpu::CpuPin<'scope>) -> R,
) -> R {
    let _guard = PreemptIrqSaveGuard::new();
    // SAFETY: the guard prevents migration for the complete callback.
    unsafe { ax_hal::percpu::with_cpu_pin(operation) }
        .expect("timer access requires an installed CPU-local area")
}

fn with_local_exclusive<R>(
    operation: impl for<'exclusive> FnOnce(&ax_hal::percpu::ExclusiveCpu<'exclusive>) -> R,
) -> R {
    let _guard = PreemptIrqSaveGuard::new();
    // SAFETY: the guard excludes migration, local IRQ/re-entry, and conflicting
    // local access for the complete callback.
    unsafe {
        ax_hal::percpu::with_cpu_pin(|pin| ax_hal::percpu::with_exclusive_cpu(pin, operation))
    }
    .expect("timer access requires an installed CPU-local area")
}

#[cfg(test)]
mod tests {
    use super::{KernelTimerCallContext, validate_kernel_timer_call_context};

    #[test]
    fn vcpu_thread_critical_section_may_update_soft_timers() {
        assert!(
            validate_kernel_timer_call_context(KernelTimerCallContext::ThreadCriticalSection)
                .is_ok()
        );
    }

    #[test]
    fn hard_irq_may_not_register_or_cancel_soft_timers() {
        assert!(validate_kernel_timer_call_context(KernelTimerCallContext::HardIrq).is_err());
    }
}
